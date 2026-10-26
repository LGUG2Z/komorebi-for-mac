//! What people and agents say about a recording: where a bug starts and ends, notes, findings.
//!
//! The most important annotation is the **bug region**: the stretch of the recording during which
//! the user saw something wrong, with a description of what it looked like. It is the ground truth
//! of *what* went wrong that every investigation starts from; the rest of the session is evidence
//! of *why*. An agent's conclusion is written back as a `finding` that points at its evidence.
//!
//! They live in `annotations.jsonl`, one JSON object per line, **append-only**: a recorder, the
//! viewer and an agent may all add to it, even while the recording is still running, and a crash
//! mid-write can only damage the last line. Changing or removing an annotation appends an `edit` or
//! `delete` line for its id; reading folds the lines in order. Times are on the master clock
//! (`*_mach_ns`), not in video seconds, so they stay right if `reindex` moves video t=0.
//!
//! A region whose end was never set is *open-ended*: the bug was still happening when the recording
//! stopped (or the recorder was killed first). Readers treat it as ending when the recording did
//! and say so.

use crate::analysis::envelope;
use crate::analysis::guard::SessionPath;
use crate::analysis::guard::is_plain_relative;
use crate::analysis::ids;
use crate::clock;
use crate::clock::ClockMap;
use crate::session::Manifest;
use crate::session::Session;
use crate::timeline;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

pub const ANNOTATIONS_FILE: &str = "annotations.jsonl";

/// The version written to every line; lines from a newer version are skipped with a warning.
pub const LINE_VERSION: u32 = 1;

/// The author recorded for a region marked with the recorder window's Mark button.
pub const AUTHOR_RECORDER_MARK: &str = "recorder-mark";
pub const AUTHOR_USER: &str = "user";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A region where the bug was visible, described by the user.
    Bug,
    Note,
    /// An agent's conclusion about what caused a bug, pointing at its evidence.
    Finding,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bug => "bug",
            Self::Note => "note",
            Self::Finding => "finding",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Add,
    Edit,
    Delete,
}

/// Evidence a finding points at. Every kind can be checked, see [`validate_refs`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Ref {
    /// A komorebi subscriber event.
    Event { seq: u64 },
    /// A line of a recorded text file (the komorebi log slice by default).
    Log { file: String, line: u64 },
    /// A line of komorebi's source, relative to the repository root.
    Code { path: String, line: u64 },
    /// A moment in the video.
    Frame { video_s: f64 },
}

/// The log slice that `log:<line>` refers to.
pub const DEFAULT_LOG_FILE: &str = "komorebi.log";

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Event { seq } => write!(f, "event:{seq}"),
            Self::Log { file, line } => write!(f, "log:{file}:{line}"),
            Self::Code { path, line } => write!(f, "code:{path}:{line}"),
            Self::Frame { video_s } => write!(f, "frame:{}", crate::timecode::format(*video_s)),
        }
    }
}

impl FromStr for Ref {
    type Err = eyre::Report;

    /// `event:12`, `log:345`, `log:oslog/komorebi.ndjson:77`, `code:komorebi/src/window.rs:210`
    /// or `frame:12.5`.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let bad = |why: &str| envelope::usage(format!("'{input}': {why}"));
        let (kind, rest) = input.split_once(':').ok_or_else(|| {
            bad("expected event:<seq>, log:<line>, code:<path>:<line> or frame:<time>")
        })?;
        // `<anything>:<line>`: the line is after the last colon
        let file_and_line = |rest: &str| -> eyre::Result<(String, u64)> {
            let (file, line) = rest
                .rsplit_once(':')
                .ok_or_else(|| bad("expected <path>:<line>"))?;
            Ok((
                file.to_string(),
                line.parse()
                    .map_err(|_| bad("the line must be a whole number"))?,
            ))
        };

        match kind {
            "event" => Ok(Self::Event {
                seq: rest.parse().map_err(|_| bad("an event sequence number"))?,
            }),
            "log" => match rest.parse::<u64>() {
                Ok(line) => Ok(Self::Log {
                    file: DEFAULT_LOG_FILE.to_string(),
                    line,
                }),
                Err(_) => {
                    let (file, line) = file_and_line(rest)?;
                    Ok(Self::Log { file, line })
                }
            },
            "code" => {
                let (path, line) = file_and_line(rest)?;
                Ok(Self::Code { path, line })
            }
            "frame" => Ok(Self::Frame {
                video_s: crate::timecode::parse(rest)?,
            }),
            _ => Err(bad("unknown kind of reference")),
        }
    }
}

/// One line of `annotations.jsonl`. Only an `add` carries everything; an `edit` carries the id and
/// whichever fields it changes, a `delete` only the id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub v: u32,
    pub id: String,
    pub op: Option<Op>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_mach_ns: Option<u64>,
    /// Once an end is set it can be moved but not cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_mach_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub displays: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// For a finding: the bug annotation it explains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<Vec<Ref>>,
    /// When the line was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_wall_us: Option<i64>,
}

/// An annotation after folding every line about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub id: String,
    pub kind: Kind,
    pub start_mach_ns: u64,
    pub end_mach_ns: Option<u64>,
    pub title: String,
    pub text: String,
    pub expected: Option<String>,
    pub displays: Vec<u32>,
    pub author: String,
    pub about: Option<String>,
    pub refs: Vec<Ref>,
    pub created_wall_us: Option<i64>,
    pub edited_wall_us: Option<i64>,
}

impl Annotation {
    /// A bug that says nothing about what went wrong is not much use to an agent.
    #[must_use]
    pub fn needs_description(&self) -> bool {
        self.kind == Kind::Bug && self.text.trim().is_empty()
    }

    /// Whether the region was still open when the recording ended.
    #[must_use]
    pub fn open_ended(&self) -> bool {
        self.end_mach_ns.is_none()
    }

    /// The end, or when the recording ended for an open region.
    #[must_use]
    pub fn end_or_session_end(&self, manifest: &Manifest) -> u64 {
        self.end_mach_ns
            .unwrap_or_else(|| crate::analysis::timespec::session_end_ns(manifest))
            .max(self.start_mach_ns)
    }
}

/// An annotation as it is shown to a reader, with both clocks filled in.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AnnotationView {
    pub id: String,
    pub kind: Kind,
    pub author: String,
    pub title: String,
    /// What the bug looks like, in the user's words.
    pub text: String,
    /// What should have happened instead.
    pub expected: Option<String>,
    /// The displays it was seen on; empty means any.
    pub displays: Vec<u32>,
    pub start_video_s: f64,
    /// The end, or the end of the recording for an open-ended region.
    pub end_video_s: f64,
    pub start_wall_utc: Option<String>,
    pub end_wall_utc: Option<String>,
    pub start_mach_ns: u64,
    pub end_mach_ns: Option<u64>,
    /// The region has no end: it was still happening when the recording stopped.
    pub open_ended: bool,
    /// A bug without a description.
    pub needs_description: bool,
    pub about: Option<String>,
    pub refs: Vec<Ref>,
}

impl Annotation {
    #[must_use]
    pub fn view(&self, manifest: &Manifest) -> AnnotationView {
        let clock = ClockMap::new(manifest.anchors.clone());
        let end_ns = self.end_or_session_end(manifest);
        let wall = |mach_ns: u64| {
            clock
                .mach_to_wall(mach_ns)
                .and_then(timeline::format_wall_us)
        };

        AnnotationView {
            id: self.id.clone(),
            kind: self.kind,
            author: self.author.clone(),
            title: self.title.clone(),
            text: self.text.clone(),
            expected: self.expected.clone(),
            displays: self.displays.clone(),
            start_video_s: manifest.video_seconds(self.start_mach_ns),
            end_video_s: manifest.video_seconds(end_ns),
            start_wall_utc: wall(self.start_mach_ns),
            end_wall_utc: wall(end_ns),
            start_mach_ns: self.start_mach_ns,
            end_mach_ns: self.end_mach_ns,
            open_ended: self.open_ended(),
            needs_description: self.needs_description(),
            about: self.about.clone(),
            refs: self.refs.clone(),
        }
    }
}

/// The annotations of a session and what was wrong with the file.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Ordered by start, then id.
    pub annotations: Vec<Annotation>,
    pub warnings: Vec<String>,
}

impl Loaded {
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Annotation> {
        self.annotations
            .iter()
            .find(|annotation| annotation.id == id)
    }

    pub fn bugs(&self) -> impl Iterator<Item = &Annotation> {
        self.annotations
            .iter()
            .filter(|annotation| annotation.kind == Kind::Bug)
    }
}

/// Fold the lines of `annotations.jsonl` into annotations. Lines that cannot be understood are
/// skipped and reported rather than failing the read: this file is written by several programs and
/// by people, and the rest of it is still worth having.
#[must_use]
pub fn fold(text: &str) -> Loaded {
    let mut live: BTreeMap<String, Annotation> = BTreeMap::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut warnings = Vec::new();

    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        if raw.trim().is_empty() {
            continue;
        }

        let line: Line = match serde_json::from_str(raw) {
            Ok(line) => line,
            Err(error) => {
                warnings.push(format!(
                    "{ANNOTATIONS_FILE} line {number} was skipped: {error}"
                ));
                continue;
            }
        };

        if line.v > LINE_VERSION {
            warnings.push(format!(
                "{ANNOTATIONS_FILE} line {number} was written by a newer komorebi-recorder (v{}) and was skipped",
                line.v
            ));
            continue;
        }

        match line.op.unwrap_or(Op::Add) {
            Op::Add => {
                let (Some(kind), Some(start)) = (line.kind, line.start_mach_ns) else {
                    warnings.push(format!(
                        "{ANNOTATIONS_FILE} line {number} adds {} without a kind and a start and was skipped",
                        line.id
                    ));
                    continue;
                };

                if live.contains_key(&line.id) || deleted.contains(&line.id) {
                    warnings.push(format!(
                        "{ANNOTATIONS_FILE} line {number} adds {} again and was skipped",
                        line.id
                    ));
                    continue;
                }

                live.insert(
                    line.id.clone(),
                    Annotation {
                        id: line.id,
                        kind,
                        start_mach_ns: start,
                        end_mach_ns: line.end_mach_ns,
                        title: line.title.unwrap_or_default(),
                        text: line.text.unwrap_or_default(),
                        expected: line.expected,
                        displays: line.displays.unwrap_or_default(),
                        author: line.author.unwrap_or_else(|| AUTHOR_USER.to_string()),
                        about: line.about,
                        refs: line.refs.unwrap_or_default(),
                        created_wall_us: line.at_wall_us,
                        edited_wall_us: None,
                    },
                );
            }
            Op::Edit => {
                let Some(annotation) = live.get_mut(&line.id) else {
                    warnings.push(format!(
                        "{ANNOTATIONS_FILE} line {number} edits {}, which does not exist, and was skipped",
                        line.id
                    ));
                    continue;
                };

                if let Some(start) = line.start_mach_ns {
                    annotation.start_mach_ns = start;
                }
                if let Some(end) = line.end_mach_ns {
                    annotation.end_mach_ns = Some(end);
                }
                if let Some(title) = line.title {
                    annotation.title = title;
                }
                if let Some(text) = line.text {
                    annotation.text = text;
                }
                if let Some(expected) = line.expected {
                    annotation.expected = Some(expected).filter(|value| !value.is_empty());
                }
                if let Some(displays) = line.displays {
                    annotation.displays = displays;
                }
                if let Some(refs) = line.refs {
                    annotation.refs = refs;
                }
                annotation.edited_wall_us = line.at_wall_us;
            }
            Op::Delete => {
                if live.remove(&line.id).is_some() {
                    deleted.push(line.id);
                } else {
                    warnings.push(format!(
                        "{ANNOTATIONS_FILE} line {number} deletes {}, which does not exist, and was skipped",
                        line.id
                    ));
                }
            }
        }
    }

    let mut annotations: Vec<Annotation> = live.into_values().collect();
    annotations.sort_by(|a, b| (a.start_mach_ns, &a.id).cmp(&(b.start_mach_ns, &b.id)));
    Loaded {
        annotations,
        warnings,
    }
}

/// Read a session's annotations. A session with none has no file, which is not an error.
pub fn load(dir: &Path) -> eyre::Result<Loaded> {
    let path = dir.join(ANNOTATIONS_FILE);
    match std::fs::read(&path) {
        // a crash can leave half a multi-byte character at the end; do not lose the rest
        Ok(bytes) => Ok(fold(&String::from_utf8_lossy(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Loaded::default()),
        Err(error) => Err(error).wrap_err_with(|| format!("could not read {}", path.display())),
    }
}

/// Append one line, atomically with respect to other writers.
///
/// The file is locked for the duration so two programs marking at once cannot interleave, and a
/// line a crashed writer left unterminated is closed off first so it cannot swallow ours.
pub fn append(dir: &Path, line: &Line) -> eyre::Result<()> {
    let path = dir.join(ANNOTATIONS_FILE);
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
        .wrap_err_with(|| format!("could not open {}", path.display()))?;

    // SAFETY: the descriptor is valid for the lifetime of `file`; the lock is released on drop.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error())
            .wrap_err_with(|| format!("could not lock {}", path.display()));
    }

    let length = file.metadata()?.len();
    let mut text = String::new();
    if length > 0 {
        let mut last = [0_u8; 1];
        file.seek(SeekFrom::Start(length - 1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            text.push('\n');
        }
    }

    text.push_str(&serde_json::to_string(line)?);
    text.push('\n');
    // one write, so a reader never sees the line in pieces
    file.write_all(text.as_bytes())?;
    file.flush()?;

    // SAFETY: as above.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    Ok(())
}

/// What to say about a new annotation.
#[derive(Debug, Clone, Default)]
pub struct New {
    pub kind: Option<Kind>,
    pub start_mach_ns: u64,
    pub end_mach_ns: Option<u64>,
    pub title: String,
    pub text: String,
    pub expected: Option<String>,
    pub displays: Vec<u32>,
    pub author: Option<String>,
    pub about: Option<String>,
    pub refs: Vec<Ref>,
}

/// Add an annotation and return its id.
pub fn add(dir: &Path, new: New) -> eyre::Result<String> {
    if let Some(end) = new.end_mach_ns
        && end < new.start_mach_ns
    {
        return Err(envelope::usage("the end of a region is before its start"));
    }

    let id = ids::new_annotation_id();
    append(
        dir,
        &Line {
            v: LINE_VERSION,
            id: id.clone(),
            op: Some(Op::Add),
            kind: Some(new.kind.unwrap_or(Kind::Note)),
            start_mach_ns: Some(new.start_mach_ns),
            end_mach_ns: new.end_mach_ns,
            title: Some(new.title),
            text: Some(new.text),
            expected: new.expected,
            displays: Some(new.displays),
            author: Some(new.author.unwrap_or_else(|| AUTHOR_USER.to_string())),
            about: new.about,
            refs: Some(new.refs),
            at_wall_us: Some(clock::wall_now_us()),
        },
    )?;

    Ok(id)
}

/// The fields of an annotation an edit may change; `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub start_mach_ns: Option<u64>,
    pub end_mach_ns: Option<u64>,
    pub title: Option<String>,
    pub text: Option<String>,
    pub expected: Option<String>,
    pub displays: Option<Vec<u32>>,
    pub refs: Option<Vec<Ref>>,
}

/// Change an annotation. Fails if it does not exist or the change would put its end before its
/// start.
pub fn edit(dir: &Path, id: &str, patch: Patch) -> eyre::Result<()> {
    let loaded = load(dir)?;
    let current = loaded
        .get(id)
        .ok_or_else(|| envelope::not_found(format!("there is no annotation {id}")))?;

    let start = patch.start_mach_ns.unwrap_or(current.start_mach_ns);
    if let Some(end) = patch.end_mach_ns.or(current.end_mach_ns)
        && end < start
    {
        return Err(envelope::usage("the end of a region is before its start"));
    }

    append(
        dir,
        &Line {
            v: LINE_VERSION,
            id: id.to_string(),
            op: Some(Op::Edit),
            start_mach_ns: patch.start_mach_ns,
            end_mach_ns: patch.end_mach_ns,
            title: patch.title,
            text: patch.text,
            expected: patch.expected,
            displays: patch.displays,
            refs: patch.refs,
            at_wall_us: Some(clock::wall_now_us()),
            ..Line::default()
        },
    )
}

/// Remove an annotation (the line stays in the file as a tombstone).
pub fn delete(dir: &Path, id: &str) -> eyre::Result<()> {
    if load(dir)?.get(id).is_none() {
        return Err(envelope::not_found(format!("there is no annotation {id}")));
    }

    append(
        dir,
        &Line {
            v: LINE_VERSION,
            id: id.to_string(),
            op: Some(Op::Delete),
            at_wall_us: Some(clock::wall_now_us()),
            ..Line::default()
        },
    )
}

/// Where to find komorebi's source to check [`Ref::Code`] against: `KOMOREBI_SOURCE`, else the
/// repository this recorder was built from if it is still on disk.
#[must_use]
pub fn default_code_root() -> Option<PathBuf> {
    let from_env = std::env::var_os("KOMOREBI_SOURCE").map(PathBuf::from);
    let from_build = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf);
    from_env
        .into_iter()
        .chain(from_build)
        .find(|root| root.join("komorebi/src").is_dir())
}

fn count_lines(path: &Path) -> std::io::Result<u64> {
    let bytes = std::fs::read(path)?;
    Ok(bytes.split(|byte| *byte == b'\n').count() as u64 - u64::from(bytes.last() == Some(&b'\n')))
}

/// Check that every piece of evidence exists: the event was recorded, the log line is in the file,
/// the video time is inside the recording, the source line is in komorebi's code. Returns warnings
/// for things that could not be checked; a reference that is definitely wrong is an error, so an
/// agent cannot cite evidence that is not there.
pub fn validate_refs(
    session: &Session,
    manifest: &Manifest,
    refs: &[Ref],
    code_root: Option<&Path>,
) -> eyre::Result<Vec<String>> {
    let mut warnings = Vec::new();
    let guard = SessionPath::new(&session.dir)?;

    for reference in refs {
        match reference {
            Ref::Event { seq } => {
                let text = std::fs::read_to_string(session.path("events.jsonl"))
                    .map_err(|_| envelope::not_found("the session recorded no events"))?;
                #[derive(Deserialize)]
                struct Seq {
                    seq: u64,
                }
                let found = text
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Seq>(line).ok())
                    .any(|row| row.seq == *seq);
                if !found {
                    return Err(envelope::not_found(format!("there is no event #{seq}")));
                }
            }
            Ref::Log { file, line } => {
                let path = guard.resolve(file)?;
                let lines = count_lines(&path)
                    .map_err(|_| envelope::not_found(format!("the session has no {file}")))?;
                if *line == 0 || *line > lines {
                    return Err(envelope::not_found(format!(
                        "{file} has {lines} lines; there is no line {line}"
                    )));
                }
            }
            Ref::Frame { video_s } => {
                let (_, end) = manifest.video_span();
                if *video_s < 0.0 || *video_s > end {
                    return Err(envelope::not_found(format!(
                        "video time {} is outside the recording (0 to {})",
                        crate::timecode::format(*video_s),
                        crate::timecode::format(end)
                    )));
                }
            }
            Ref::Code { path, line } => {
                if !is_plain_relative(path) {
                    return Err(envelope::usage(format!(
                        "'{path}' must be a path relative to the komorebi repository"
                    )));
                }

                let Some(root) = code_root else {
                    warnings.push(format!(
                        "code:{path}:{line} could not be checked: komorebi's source was not found \
                         (set KOMOREBI_SOURCE to the repository)"
                    ));
                    continue;
                };

                let lines = count_lines(&root.join(path)).map_err(|_| {
                    envelope::not_found(format!("{} has no file {path}", root.display()))
                })?;
                if *line == 0 || *line > lines {
                    return Err(envelope::not_found(format!(
                        "{path} has {lines} lines; there is no line {line}"
                    )));
                }
            }
        }
    }

    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Anchor;
    use crate::session::Moment;
    use chrono::Utc;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-annotations-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn bug(start: u64, end: Option<u64>, text: &str) -> New {
        New {
            kind: Some(Kind::Bug),
            start_mach_ns: start,
            end_mach_ns: end,
            text: text.to_string(),
            ..New::default()
        }
    }

    #[test]
    fn a_region_has_a_start_an_end_and_a_description() {
        let dir = temp("region");
        let id = add(
            &dir,
            bug(2_000, Some(5_000), "the window jumps to the wrong monitor"),
        )
        .expect("add");

        let loaded = load(&dir).expect("load");
        let annotation = loaded.get(&id).expect("annotation");
        assert_eq!(annotation.kind, Kind::Bug);
        assert_eq!(annotation.start_mach_ns, 2_000);
        assert_eq!(annotation.end_mach_ns, Some(5_000));
        assert_eq!(annotation.text, "the window jumps to the wrong monitor");
        assert!(!annotation.open_ended());
        assert!(!annotation.needs_description());
        assert_eq!(annotation.author, AUTHOR_USER);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn a_region_can_be_opened_now_and_closed_later() {
        let dir = temp("open-close");
        let id = add(&dir, bug(2_000, None, "")).expect("start");

        let open = load(&dir).expect("load");
        assert!(open.get(&id).expect("open").open_ended());
        assert!(open.get(&id).expect("open").needs_description());

        edit(
            &dir,
            &id,
            Patch {
                end_mach_ns: Some(9_000),
                text: Some("tiles overlap after the dock".to_string()),
                ..Patch::default()
            },
        )
        .expect("close");

        let closed = load(&dir).expect("load");
        let annotation = closed.get(&id).expect("closed");
        assert_eq!(annotation.end_mach_ns, Some(9_000));
        assert_eq!(annotation.text, "tiles overlap after the dock");
        assert!(annotation.edited_wall_us.is_some());
        assert!(!annotation.needs_description());
    }

    #[test]
    fn an_end_before_the_start_is_refused() {
        let dir = temp("backwards");
        assert!(add(&dir, bug(5_000, Some(2_000), "x")).is_err());

        let id = add(&dir, bug(5_000, None, "x")).expect("add");
        let error = edit(
            &dir,
            &id,
            Patch {
                end_mach_ns: Some(1_000),
                ..Patch::default()
            },
        )
        .expect_err("backwards");
        assert_eq!(envelope::exit_kind(&error), envelope::ExitKind::Usage);
    }

    #[test]
    fn deleting_leaves_a_tombstone_and_hides_the_annotation() {
        let dir = temp("delete");
        let keep = add(&dir, bug(1_000, Some(2_000), "keep")).expect("keep");
        let drop = add(&dir, bug(3_000, Some(4_000), "drop")).expect("drop");

        delete(&dir, &drop).expect("delete");

        let loaded = load(&dir).expect("load");
        assert_eq!(loaded.annotations.len(), 1);
        assert!(loaded.get(&keep).is_some());
        // append-only: three lines, nothing rewritten
        let text = std::fs::read_to_string(dir.join(ANNOTATIONS_FILE)).expect("file");
        assert_eq!(text.lines().count(), 3);
        // it cannot be deleted or edited twice
        assert!(delete(&dir, &drop).is_err());
        assert!(edit(&dir, &drop, Patch::default()).is_err());
    }

    #[test]
    fn annotations_are_ordered_by_start() {
        let dir = temp("order");
        add(&dir, bug(9_000, None, "later")).expect("later");
        add(&dir, bug(1_000, None, "earlier")).expect("earlier");

        let starts: Vec<u64> = load(&dir)
            .expect("load")
            .annotations
            .iter()
            .map(|annotation| annotation.start_mach_ns)
            .collect();
        assert_eq!(starts, [1_000, 9_000]);
    }

    #[test]
    fn a_missing_file_is_an_empty_session_not_an_error() {
        let loaded = load(&temp("missing")).expect("load");
        assert!(loaded.annotations.is_empty());
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn damaged_lines_are_skipped_and_reported_without_losing_the_rest() {
        let good = |id: &str, start: u64| {
            serde_json::to_string(&Line {
                v: LINE_VERSION,
                id: id.to_string(),
                op: Some(Op::Add),
                kind: Some(Kind::Bug),
                start_mach_ns: Some(start),
                ..Line::default()
            })
            .expect("line")
        };
        let text = format!(
            "{}\nthis is not json\n{{\"v\":1,\"id\":\"a-00000009\",\"op\":\"edit\",\"title\":\"x\"}}\n\
             {{\"v\":99,\"id\":\"a-0000000a\",\"op\":\"add\",\"kind\":\"bug\",\"start_mach_ns\":1}}\n\
             {{\"v\":1,\"id\":\"a-0000000b\",\"op\":\"add\"}}\n{}\n{}",
            good("a-00000001", 1),
            good("a-00000001", 2),
            good("a-00000002", 3),
        );

        let loaded = fold(&text);
        let ids: Vec<&str> = loaded.annotations.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["a-00000001", "a-00000002"]);
        // the duplicate add did not overwrite the first
        assert_eq!(loaded.annotations[0].start_mach_ns, 1);
        assert_eq!(loaded.warnings.len(), 5, "{:#?}", loaded.warnings);
    }

    #[test]
    fn a_line_a_crashed_writer_left_unterminated_does_not_swallow_the_next() {
        let dir = temp("torn");
        std::fs::write(dir.join(ANNOTATIONS_FILE), "{\"v\":1,\"id\":\"a-0000").expect("torn");

        let id = add(&dir, bug(1_000, None, "after the crash")).expect("add");
        let loaded = load(&dir).expect("load");

        assert_eq!(loaded.annotations.len(), 1);
        assert_eq!(loaded.annotations[0].id, id);
        assert_eq!(loaded.warnings.len(), 1, "the torn line is reported");
    }

    #[test]
    fn concurrent_writers_do_not_interleave_lines() {
        let dir = temp("concurrent");
        let threads: Vec<_> = (0..8)
            .map(|worker| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for index in 0..25 {
                        add(&dir, bug(worker * 1000 + index, None, &"x".repeat(2000)))
                            .expect("add");
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("join");
        }

        let loaded = load(&dir).expect("load");
        assert_eq!(loaded.annotations.len(), 200);
        assert!(loaded.warnings.is_empty(), "{:#?}", loaded.warnings);
    }

    fn manifest() -> Manifest {
        let mut manifest = Manifest::new(
            Moment {
                mach_ns: 1_000_000_000,
                wall_us: 1_000_000,
            },
            vec![Anchor {
                mach_ns: 1_000_000_000,
                wall_us: 1_000_000,
            }],
        );
        manifest.video_t0_mach_ns = Some(1_000_000_000);
        manifest.stop = Some(Moment {
            mach_ns: 11_000_000_000,
            wall_us: 11_000_000,
        });
        manifest
    }

    #[test]
    fn the_view_has_both_clocks_and_closes_an_open_region_at_the_end() {
        let manifest = manifest();
        let annotation = Annotation {
            id: "a-00000001".to_string(),
            kind: Kind::Bug,
            start_mach_ns: 3_000_000_000,
            end_mach_ns: None,
            title: String::new(),
            text: String::new(),
            expected: None,
            displays: vec![],
            author: AUTHOR_USER.to_string(),
            about: None,
            refs: vec![],
            created_wall_us: None,
            edited_wall_us: None,
        };

        let view = annotation.view(&manifest);
        assert!((view.start_video_s - 2.0).abs() < 1e-9);
        assert!((view.end_video_s - 10.0).abs() < 1e-9);
        assert!(view.open_ended);
        assert!(view.needs_description);
        assert_eq!(
            view.start_wall_utc.as_deref(),
            Some("1970-01-01T00:00:03.000000Z")
        );
    }

    #[test]
    fn references_parse_and_print() {
        for (text, expected) in [
            ("event:12", Ref::Event { seq: 12 }),
            (
                "log:345",
                Ref::Log {
                    file: DEFAULT_LOG_FILE.to_string(),
                    line: 345,
                },
            ),
            (
                "log:oslog/komorebi.ndjson:77",
                Ref::Log {
                    file: "oslog/komorebi.ndjson".to_string(),
                    line: 77,
                },
            ),
            (
                "code:komorebi/src/window.rs:210",
                Ref::Code {
                    path: "komorebi/src/window.rs".to_string(),
                    line: 210,
                },
            ),
            ("frame:12.5", Ref::Frame { video_s: 12.5 }),
        ] {
            assert_eq!(text.parse::<Ref>().expect(text), expected);
        }

        assert_eq!(
            "log:oslog/komorebi.ndjson:77"
                .parse::<Ref>()
                .expect("ref")
                .to_string(),
            "log:oslog/komorebi.ndjson:77"
        );
        for bad in ["event:x", "code:no-line", "nonsense", "frame:", "cat:1"] {
            assert!(bad.parse::<Ref>().is_err(), "{bad}");
        }
    }

    fn session_with_evidence(name: &str) -> (Session, Manifest) {
        let dir = temp(name);
        let session = Session::create(Some(dir.clone()), Utc::now()).expect("create");
        let manifest = manifest();
        session.write_manifest(&manifest).expect("manifest");
        std::fs::write(dir.join("komorebi.log"), "one\ntwo\nthree\n").expect("log");
        std::fs::write(
            dir.join("events.jsonl"),
            "{\"seq\":1,\"recv_wall_us\":0,\"recv_mach_ns\":0}\n{\"seq\":2,\"recv_wall_us\":0,\"recv_mach_ns\":0}\n",
        )
        .expect("events");
        (session, manifest)
    }

    #[test]
    fn evidence_that_exists_is_accepted() {
        let (session, manifest) = session_with_evidence("evidence-ok");
        let root = temp("code-root");
        std::fs::create_dir_all(root.join("komorebi/src")).expect("src");
        std::fs::write(root.join("komorebi/src/window.rs"), "a\nb\n").expect("file");

        let warnings = validate_refs(
            &session,
            &manifest,
            &[
                Ref::Event { seq: 2 },
                Ref::Log {
                    file: "komorebi.log".to_string(),
                    line: 3,
                },
                Ref::Frame { video_s: 4.0 },
                Ref::Code {
                    path: "komorebi/src/window.rs".to_string(),
                    line: 2,
                },
            ],
            Some(&root),
        )
        .expect("valid");
        assert!(warnings.is_empty());
    }

    #[test]
    fn evidence_that_is_not_there_is_refused() {
        let (session, manifest) = session_with_evidence("evidence-bad");
        let root = temp("code-root-bad");
        std::fs::create_dir_all(root.join("komorebi/src")).expect("src");
        std::fs::write(root.join("komorebi/src/window.rs"), "a\nb\n").expect("file");

        for reference in [
            Ref::Event { seq: 3 },
            Ref::Log {
                file: "komorebi.log".to_string(),
                line: 4,
            },
            Ref::Log {
                file: "../escape".to_string(),
                line: 1,
            },
            Ref::Log {
                file: "komorebi.log".to_string(),
                line: 0,
            },
            Ref::Frame { video_s: 99.0 },
            Ref::Code {
                path: "komorebi/src/window.rs".to_string(),
                line: 3,
            },
            Ref::Code {
                path: "komorebi/src/missing.rs".to_string(),
                line: 1,
            },
            Ref::Code {
                path: "../x.rs".to_string(),
                line: 1,
            },
        ] {
            assert!(
                validate_refs(
                    &session,
                    &manifest,
                    std::slice::from_ref(&reference),
                    Some(&root)
                )
                .is_err(),
                "{reference} should be refused"
            );
        }
    }

    #[test]
    fn source_that_cannot_be_checked_is_a_warning_not_a_pass() {
        let (session, manifest) = session_with_evidence("evidence-unchecked");
        let warnings = validate_refs(
            &session,
            &manifest,
            &[Ref::Code {
                path: "komorebi/src/window.rs".to_string(),
                line: 2,
            }],
            None,
        )
        .expect("not an error");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("KOMOREBI_SOURCE"));
    }
}

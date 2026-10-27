//! Which line of komorebi's source wrote a log line.
//!
//! `build.rs` indexes every `tracing` statement in komorebi's source at the commit the recorder was
//! built from. A recorded line is matched to those statements by module, level and message: the
//! message of a statement is a format string, so `reaping window: {window_id}` matches the line
//! `reaping window: 4711`.
//!
//! A match is only as good as the source it was made against. The recording says which komorebi was
//! running (see [`crate::sources::komorebi_version`]) and the catalog knows its own commit, so a
//! mismatch is reported with every answer instead of letting a line be pinned on code that may not
//! be what ran.

use super::logparse::Level;
use crate::sources::komorebi_version::KomorebiVersion;
use schemars::JsonSchema;
use serde::Serialize;

/// A log statement as `build.rs` wrote it.
pub struct RawEntry {
    pub level: &'static str,
    pub target: &'static str,
    pub template: &'static str,
    pub file: &'static str,
    pub line: usize,
    pub function: &'static str,
    pub owner: &'static str,
}

/// An `#[instrument]`ed function: the span its log lines carry.
pub struct RawSpan {
    pub name: &'static str,
    pub file: &'static str,
    pub line: usize,
    pub function: &'static str,
    pub owner: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/log_catalog.rs"));

/// The commit of komorebi's source that was indexed, or `None` if it was not a git checkout.
#[must_use]
pub fn commit() -> Option<&'static str> {
    (!COMMIT.is_empty()).then_some(COMMIT)
}

/// Whether the source had uncommitted changes when it was indexed.
#[must_use]
pub fn dirty() -> bool {
    DIRTY
}

/// How many source files were read to build the catalog.
#[must_use]
pub fn files_scanned() -> usize {
    FILES_SCANNED
}

/// How many statements were indexed.
#[must_use]
pub fn len() -> usize {
    ENTRIES.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    /// The message is the statement's whole text and nothing else matched.
    Exact,
    /// A statement with distinctive text matched, and nothing else did as well.
    Likely,
    /// Several statements matched equally well.
    Ambiguous,
    /// Only a statement made of placeholders (`"{}"`) matched, so it says little.
    Generic,
}

/// A statement of komorebi's source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Statement {
    pub level: String,
    pub target: String,
    /// The statement's format string, e.g. `reaping window: {window_id}`.
    pub template: String,
    /// Relative to the repository root.
    pub file: String,
    pub line: usize,
    /// The function the statement is in.
    pub function: String,
    /// The type whose `impl` it is in, if any.
    pub owner: String,
}

impl Statement {
    fn of(entry: &RawEntry) -> Self {
        Self {
            level: entry.level.to_string(),
            target: entry.target.to_string(),
            template: entry.template.to_string(),
            file: entry.file.to_string(),
            line: entry.line,
            function: entry.function.to_string(),
            owner: entry.owner.to_string(),
        }
    }

    /// `file:line`, as an editor opens it.
    #[must_use]
    pub fn location(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }

    /// `WindowManager::process_event`.
    #[must_use]
    pub fn qualified_function(&self) -> String {
        if self.owner.is_empty() {
            self.function.clone()
        } else {
            format!("{}::{}", self.owner, self.function)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Source {
    pub statement: Statement,
    pub confidence: Confidence,
    /// Other statements that matched as well as this one.
    pub alternatives: Vec<Statement>,
}

/// The literal pieces of a format string around its placeholders: `a {} b {x:?} c` is
/// `["a ", " b ", " c"]`. `{{` and `}}` are literal braces.
fn literals(template: &str) -> Vec<String> {
    let mut pieces = vec![String::new()];
    let mut characters = template.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                characters.next();
                pieces.last_mut().expect("never empty").push('{');
            }
            '}' if characters.peek() == Some(&'}') => {
                characters.next();
                pieces.last_mut().expect("never empty").push('}');
            }
            '{' => {
                for inner in characters.by_ref() {
                    if inner == '}' {
                        break;
                    }
                }
                pieces.push(String::new());
            }
            other => pieces.last_mut().expect("never empty").push(other),
        }
    }

    pieces
}

/// How much literal text of `template` `message` shares, or `None` if it cannot be an expansion of
/// it. The literal pieces must appear in order, the first at the start and the last at the end.
fn score(template: &str, message: &str) -> Option<usize> {
    let pieces = literals(template);
    if pieces.len() == 1 {
        return (pieces[0] == message).then(|| pieces[0].len() * 2 + 1);
    }

    let (first, last) = (&pieces[0], &pieces[pieces.len() - 1]);
    if !message.starts_with(first.as_str())
        || message.len() < first.len() + last.len()
        || !message.ends_with(last.as_str())
    {
        return None;
    }

    let mut rest = &message[first.len()..message.len() - last.len()];
    for piece in &pieces[1..pieces.len() - 1] {
        let at = rest.find(piece.as_str())?;
        rest = &rest[at + piece.len()..];
    }

    Some(pieces.iter().map(String::len).sum())
}

/// Find the statement that wrote a log line.
#[must_use]
pub fn find(target: &str, level: Level, message: &str) -> Option<Source> {
    let level_name = match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN",
        Level::Info => "INFO",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    };

    let mut matches: Vec<(usize, &RawEntry)> = ENTRIES
        .iter()
        .filter(|entry| entry.target == target && entry.level == level_name)
        .filter_map(|entry| score(entry.template, message).map(|score| (score, entry)))
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0));

    let (best_score, best) = *matches.first()?;
    let tied: Vec<&(usize, &RawEntry)> = matches
        .iter()
        .skip(1)
        .filter(|(score, _)| *score == best_score)
        .collect();

    let confidence = if best_score <= 3 {
        // statements made of placeholders match any message; ties among them are expected
        Confidence::Generic
    } else if !tied.is_empty() {
        Confidence::Ambiguous
    } else if literals(best.template).len() == 1 {
        Confidence::Exact
    } else {
        Confidence::Likely
    };

    Some(Source {
        statement: Statement::of(best),
        confidence,
        alternatives: tied.iter().map(|(_, entry)| Statement::of(entry)).collect(),
    })
}

/// Where the span of a function is declared: `process_event` is the `#[instrument]` on
/// `WindowManager::process_event`.
#[must_use]
pub fn span(name: &str) -> Vec<Statement> {
    SPANS
        .iter()
        .filter(|span| span.name == name)
        .map(|span| Statement {
            level: String::new(),
            target: String::new(),
            template: format!("#[instrument] {}", span.name),
            file: span.file.to_string(),
            line: span.line,
            function: span.function.to_string(),
            owner: span.owner.to_string(),
        })
        .collect()
}

/// Every statement of a module, in source order.
#[must_use]
pub fn statements_in(target: &str) -> Vec<Statement> {
    ENTRIES
        .iter()
        .filter(|entry| entry.target == target)
        .map(Statement::of)
        .collect()
}

/// Statements whose message contains `needle` (already lowercased), in source order.
#[must_use]
pub fn statements_matching(needle: &str) -> Vec<Statement> {
    ENTRIES
        .iter()
        .filter(|entry| entry.template.to_lowercase().contains(needle))
        .map(Statement::of)
        .collect()
}

/// What a part of komorebi is, for someone reading its log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Role {
    pub summary: &'static str,
    /// It changes komorebi's state without sending a subscriber notification, so a change in state
    /// with no event before it may be its doing.
    pub changes_state_silently: bool,
}

const ROLES: &[(&str, &str, bool)] = &[
    (
        "komorebi::process_event",
        "handles events from the OS one at a time under the window-manager lock; the subscriber notification is sent after the handler body",
        false,
    ),
    (
        "komorebi::process_command",
        "handles commands from komorebic or a hotkey daemon one at a time under the lock; notifies subscribers before it logs 'processed command'",
        false,
    ),
    (
        "komorebi::window_manager",
        "window-manager operations, called from the handlers",
        false,
    ),
    (
        "komorebi::workspace",
        "operations on a workspace's containers: layout, focus, adding and removing",
        false,
    ),
    (
        "komorebi::container",
        "operations on a container and its tabbed windows",
        false,
    ),
    (
        "komorebi::monitor",
        "operations on a monitor and its workspaces",
        false,
    ),
    (
        "komorebi::window",
        "operations on one window, through the accessibility API",
        false,
    ),
    (
        "komorebi::macos_api",
        "calls into macOS; an AXError comes from the target application or the OS, not from komorebi",
        false,
    ),
    (
        "komorebi::accessibility",
        "calls into the macOS accessibility API; an AXError comes from the target application or the OS",
        false,
    ),
    (
        "komorebi::application",
        "per-application accessibility observers",
        false,
    ),
    (
        "komorebi::border_manager",
        "draws window borders on its own thread; cosmetic, never changes the layout state",
        false,
    ),
    (
        "komorebi::reaper",
        "background thread that removes windows that no longer exist; it takes the lock and changes state without sending a notification",
        true,
    ),
    (
        "komorebi::workspace_reconciliator",
        "background thread that moves windows between workspaces after a space change; it takes the lock and changes state without sending a notification",
        true,
    ),
    (
        "komorebi::monitor_reconciliator",
        "background thread that reacts to displays connecting, disconnecting and resizing; it sends a monitor notification when done",
        false,
    ),
    (
        "komorebi::display_reconfiguration_listener",
        "macOS callback for display changes, feeding the monitor reconciliator",
        false,
    ),
    (
        "komorebi::static_config",
        "loads and applies komorebi.json",
        false,
    ),
    (
        "komorebi::theme_manager",
        "applies border and bar themes",
        false,
    ),
    (
        "komorebi::input_event_listener",
        "watches the mouse and keyboard for the hacks that need them",
        false,
    ),
    (
        "komorebi::window_manager_event_listener",
        "receives OS notifications and queues them for the event handler",
        false,
    ),
    (
        "komorebi::notification_center_listener",
        "receives macOS workspace notifications (app activation, space change, screen lock)",
        false,
    ),
];

/// What the part of komorebi that logs from `target` does.
#[must_use]
pub fn role(target: &str) -> Role {
    ROLES
        .iter()
        .filter(|(module, _, _)| target == *module || target.starts_with(&format!("{module}::")))
        .max_by_key(|(module, _, _)| module.len())
        .map_or(
            Role {
                summary: "komorebi internals",
                changes_state_silently: false,
            },
            |(_, summary, silent)| Role {
                summary,
                changes_state_silently: *silent,
            },
        )
}

/// How the source this recorder was built against compares with the komorebi that was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Skew {
    /// The commit the recorded komorebi was built from.
    pub recorded_commit: Option<String>,
    /// The commit whose source was indexed.
    pub indexed_commit: Option<String>,
    /// Whether the source tree had uncommitted changes when indexed.
    pub indexed_dirty: bool,
    /// `same` (the source that ran and the source indexed are identical), `different` or
    /// `unknown`.
    pub verdict: String,
    /// The files of komorebi's source that differ between the recorded commit and the indexed
    /// one. A log line mapped into any other file is as good as if the commits were the same.
    pub changed_files: Vec<String>,
    /// What to say to the reader, `None` when the two agree.
    pub warning: Option<String>,
}

impl Skew {
    /// Whether a statement in `file` may be different from the one that ran.
    #[must_use]
    pub fn file_changed(&self, file: &str) -> bool {
        self.verdict == "unknown" || self.changed_files.iter().any(|changed| changed == file)
    }
}

/// The paths whose source the catalog indexes.
const SOURCE_PATHS: [&str; 2] = ["komorebi/src", "komorebi-client/src"];

fn git(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The files of komorebi's source that differ between two commits, or `None` if either commit is
/// not in the repository.
fn changed_files(root: &std::path::Path, from: &str, to: &str) -> Option<Vec<String>> {
    for commit in [from, to] {
        git(root, &["cat-file", "-e", &format!("{commit}^{{commit}}")])?;
    }

    let mut args = vec!["diff", "--name-only", from, to, "--"];
    args.extend(SOURCE_PATHS);
    let names = git(root, &args)?;
    Some(names.lines().map(str::to_string).collect())
}

/// Compare the komorebi that ran (`versions`, as recorded) with the source that was indexed, using
/// the repository at `root` to say which files differ.
#[must_use]
pub fn skew_between(
    versions: &[KomorebiVersion],
    indexed: Option<&str>,
    dirty: bool,
    root: Option<&std::path::Path>,
) -> Skew {
    let recorded = versions.iter().find_map(|version| version.commit.clone());
    let short = |commit: &str| commit[..commit.len().min(10)].to_string();

    let mut skew = Skew {
        recorded_commit: recorded.clone(),
        indexed_commit: indexed.map(str::to_string),
        indexed_dirty: dirty,
        verdict: "unknown".to_string(),
        changed_files: Vec::new(),
        warning: None,
    };

    let (Some(recorded), Some(indexed)) = (recorded, indexed) else {
        skew.warning = Some(
            "which komorebi was recorded (or which source was indexed) is unknown, so source \
             locations may not match what ran"
                .to_string(),
        );
        return skew;
    };

    let changed = if recorded.starts_with(indexed) || indexed.starts_with(&recorded) {
        Some(Vec::new())
    } else {
        root.and_then(|root| changed_files(root, &recorded, indexed))
    };

    match changed {
        None => {
            skew.warning = Some(format!(
                "the recorded komorebi was built from {} but its source is indexed at {}, and the \
                 repository cannot say how they differ (is the recorded commit fetched?): source \
                 locations may not match what ran",
                short(&recorded),
                short(indexed)
            ));
        }
        Some(files) if files.is_empty() && !dirty => skew.verdict = "same".to_string(),
        Some(files) => {
            skew.verdict = "different".to_string();
            skew.warning = Some(format!(
                "the recorded komorebi was built from {} and its source is indexed at {}{}: {}. \
                 Log lines mapped into those files may not be what ran; lines in other files are \
                 unaffected. Check out the recorded commit and rebuild the recorder to be sure",
                short(&recorded),
                short(indexed),
                if dirty {
                    " with uncommitted changes"
                } else {
                    ""
                },
                if files.is_empty() {
                    "the working tree has uncommitted changes to it".to_string()
                } else {
                    format!(
                        "{} file(s) differ ({})",
                        files.len(),
                        files
                            .iter()
                            .map(|file| file.rsplit('/').next().unwrap_or(file))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ));
            skew.changed_files = files;
        }
    }

    skew
}

/// [`skew_between`] for the source this recorder was built from.
#[must_use]
pub fn skew(versions: &[KomorebiVersion]) -> Skew {
    skew_between(
        versions,
        commit(),
        dirty(),
        crate::annotations::default_code_root().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn templates_split_around_their_placeholders() {
        assert_eq!(literals("plain"), ["plain"]);
        assert_eq!(literals("a {} b {x:?} c"), ["a ", " b ", " c"]);
        assert_eq!(literals("{}"), ["", ""]);
        assert_eq!(literals("{{braces}} {x}"), ["{braces} ", ""]);
    }

    #[test]
    fn a_message_matches_the_template_it_expands() {
        assert_eq!(
            score("reaping window: {window_id}", "reaping window: 4711"),
            Some(16)
        );
        assert!(score("reaping window: {window_id}", "removing window: 4711").is_none());
        assert!(score("a {} b {} c", "a 1 b 2 c").is_some());
        assert!(score("a {} b {} c", "a 1 c b 2").is_none());
        // exact templates must match whole
        assert!(score("focusing window", "focusing window").is_some());
        assert!(score("focusing window", "focusing window now").is_none());
        // the start and the end both have to be there
        assert!(score("x {} y", "x 1 z").is_none());
        assert!(score("x {} y", "w 1 y").is_none());
    }

    #[test]
    fn the_catalog_was_built_from_komorebis_source() {
        assert!(
            files_scanned() > 20,
            "only {} files were indexed",
            files_scanned()
        );
        assert!(len() > 200, "only {} log statements were indexed", len());
        assert!(SPANS.len() > 5);
    }

    #[test]
    fn the_handler_header_is_found_at_its_line() {
        let found = find(
            "komorebi::process_event",
            Level::Info,
            "processing event: MoveEnd for process 20934 with notification AXWindowMoved",
        )
        .expect("the header statement");

        assert_eq!(found.statement.file, "komorebi/src/process_event.rs");
        assert_eq!(found.statement.function, "process_event");
        assert_eq!(found.statement.owner, "WindowManager");
        assert_eq!(found.confidence, Confidence::Likely);
        assert!(found.statement.line > 100);

        // and the line it names really is that statement
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("repo")
                .join(&found.statement.file),
        )
        .expect("source");
        let text = source
            .lines()
            .nth(found.statement.line - 1)
            .expect("the line exists");
        assert!(text.contains("tracing::info!"), "{text}");
    }

    #[test]
    fn a_line_that_could_come_from_two_statements_says_so() {
        // two statements of the same level in one module with the same text
        let mut seen: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
        for entry in ENTRIES
            .iter()
            .filter(|entry| !entry.template.contains('{') && entry.template.len() > 3)
        {
            *seen
                .entry((entry.target, entry.level, entry.template))
                .or_default() += 1;
        }

        let duplicate = seen
            .into_iter()
            .find(|(_, count)| *count > 1)
            .map(|((target, level, template), _)| (target, level, template));
        if let Some((target, level, template)) = duplicate {
            let level = match level {
                "ERROR" => Level::Error,
                "WARN" => Level::Warn,
                "INFO" => Level::Info,
                "DEBUG" => Level::Debug,
                _ => Level::Trace,
            };
            let found = find(target, level, template).expect("found");
            assert_eq!(found.confidence, Confidence::Ambiguous);
            assert!(!found.alternatives.is_empty());
        }
    }

    #[test]
    fn a_statement_that_is_all_placeholders_is_generic() {
        // `tracing::error!("{}", error)` in the process_event listener
        let found = find(
            "komorebi::process_event",
            Level::Error,
            "something went wrong",
        )
        .expect("a generic statement");
        assert_eq!(found.confidence, Confidence::Generic);
    }

    #[test]
    fn nothing_is_found_for_a_message_no_statement_could_write() {
        assert!(
            find(
                "komorebi::process_event",
                Level::Info,
                "no such message at all"
            )
            .is_none()
        );
        assert!(find("komorebi::nowhere", Level::Info, "processing event: x").is_none());
    }

    #[test]
    fn every_statement_of_the_real_log_sample_is_found_in_the_source() {
        let sample = include_str!("../../tests/fixtures/komorebi_plaintext_sample.log");
        let lines = crate::analysis::logparse::parse(sample.as_bytes());
        let structured: Vec<_> = lines.iter().filter(|line| !line.continuation).collect();

        let missing: Vec<String> = structured
            .iter()
            .filter(|line| {
                let (Some(target), Some(level)) = (&line.target, line.level) else {
                    return false;
                };
                find(target, level, &line.message).is_none()
            })
            .map(|line| format!("{}: {}", line.number, line.message))
            .collect();

        // the sample is from the installed binary, which can be a different commit than the
        // source: a few lines may have moved or changed, but the catalog has to be broadly right
        assert!(
            missing.len() * 10 <= structured.len(),
            "{} of {} lines were not found: {missing:#?}",
            missing.len(),
            structured.len()
        );
    }

    #[test]
    fn roles_are_found_by_the_longest_module_prefix() {
        assert!(role("komorebi::reaper").changes_state_silently);
        assert!(role("komorebi::workspace_reconciliator").changes_state_silently);
        assert!(!role("komorebi::process_event").changes_state_silently);
        assert!(
            role("komorebi::accessibility::error")
                .summary
                .contains("accessibility")
        );
        assert_eq!(role("komorebi::brand_new").summary, "komorebi internals");
    }

    fn version(commit: &str) -> KomorebiVersion {
        KomorebiVersion {
            pid: None,
            version: "0.1.0".to_string(),
            branch: None,
            commit: Some(commit.to_string()),
        }
    }

    fn repo() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo")
            .to_path_buf()
    }

    #[test]
    fn an_unknown_komorebi_or_source_is_unknown_not_assumed_the_same() {
        assert_eq!(
            skew_between(&[], Some("abc1234"), false, None).verdict,
            "unknown"
        );
        assert!(
            skew_between(&[], Some("abc1234"), false, None)
                .warning
                .is_some()
        );
        assert_eq!(
            skew_between(&[version("abc1234")], None, false, None).verdict,
            "unknown"
        );
    }

    #[test]
    fn the_same_commit_is_the_same_source() {
        let skew = skew_between(&[version("abc1234def")], Some("abc1234def"), false, None);
        assert_eq!(skew.verdict, "same");
        assert!(skew.warning.is_none());
        assert!(!skew.file_changed("komorebi/src/window.rs"));

        // but not if the source had uncommitted changes when indexed
        let dirty = skew_between(
            &[version("abc1234def")],
            Some("abc1234def"),
            true,
            Some(&repo()),
        );
        assert_eq!(dirty.verdict, "different");
    }

    #[test]
    fn a_commit_the_repository_does_not_have_is_unknown() {
        let skew = skew_between(
            &[version("0000000000000000000000000000000000000000")],
            commit(),
            false,
            Some(&repo()),
        );
        assert_eq!(skew.verdict, "unknown");
        assert!(
            skew.warning
                .as_deref()
                .expect("warning")
                .contains("cannot say how they differ")
        );
        // when it cannot be told, nothing is vouched for
        assert!(skew.file_changed("komorebi/src/window.rs"));
    }

    #[test]
    fn an_older_commit_differs_in_exactly_the_files_git_says() {
        let Some(indexed) = commit() else { return };
        // the commit before the latest change to komorebi's source
        let Some(history) = git(&repo(), &["log", "-2", "--format=%H", "--", "komorebi/src"])
        else {
            return;
        };
        let Some(older) = history.lines().nth(1) else {
            return;
        };

        let skew = skew_between(&[version(older)], Some(indexed), false, Some(&repo()));
        let expected = git(
            &repo(),
            &[
                "diff",
                "--name-only",
                older,
                indexed,
                "--",
                "komorebi/src",
                "komorebi-client/src",
            ],
        )
        .expect("git diff");
        assert_eq!(skew.changed_files.join("\n"), expected);
        assert_eq!(
            skew.verdict,
            if expected.is_empty() {
                "same"
            } else {
                "different"
            }
        );
        for file in expected.lines() {
            assert!(skew.file_changed(file));
        }
        assert!(!skew.file_changed("komorebi/src/a_file_that_never_changed.rs"));
    }

    fn rust_files(directory: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(directory).expect("read_dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }

    /// If komorebi gains a way of logging the index cannot see (a macro that wraps `tracing`, a
    /// log statement inside another macro), the count of statements in the source stops matching
    /// what was indexed and this fails, rather than `code` silently missing lines.
    #[test]
    fn every_log_statement_in_komorebis_source_is_indexed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("repo")
            .join("komorebi/src");
        let mut files = Vec::new();
        rust_files(&root, &mut files);

        let mut in_source = 0_usize;
        for file in files {
            for line in std::fs::read_to_string(&file).expect("read").lines() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for level in ["error", "warn", "info", "debug", "trace"] {
                    in_source += code.matches(&format!("tracing::{level}!")).count();
                }
            }
        }

        let indexed = ENTRIES
            .iter()
            .filter(|entry| entry.file.starts_with("komorebi/src/"))
            .count();
        assert_eq!(
            indexed, in_source,
            "{in_source} tracing statements in komorebi's source but {indexed} were indexed"
        );
    }
}

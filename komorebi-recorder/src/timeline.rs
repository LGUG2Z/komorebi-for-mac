//! The merged, sorted index over every raw file in a session, and the queries on top of it.
//!
//! `timeline.jsonl` holds one line per record: its time on every clock plus a *reference*
//! (`file`, `offset`, `len`) into a raw file. Raw data is never copied or rewritten, so the index
//! can be rebuilt at any time (`reindex`) and the streams stay separate.
//!
//! Queries binary-search the sorted file by byte offset, so asking about one second of a
//! multi-gigabyte session does not load the index into memory.

use crate::clock::ClockMap;
use crate::incidents::INCIDENTS_FILE;
use crate::incidents::Incident;
use crate::session::Manifest;
use crate::session::Session;
use crate::session::TIMELINE_FILE;
use crate::sources::komorebi_events::EventRow;
use crate::sources::komorebi_log;
use crate::sources::managed_apps;
use crate::sources::oslog;
use crate::sources::oslog::OsLogRecord;
use chrono::DateTime;
use chrono::Utc;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;

pub const SOURCE_KOMOREBI_LOG: &str = "komorebi-log";
pub const SOURCE_EVENTS: &str = "events";
pub use crate::incidents::SOURCE_RECORDER;

/// One line of `timeline.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// Seconds since the first video frame (negative before it).
    pub video_s: f64,
    pub mach_ns: u64,
    /// RFC 3339 UTC with microseconds.
    pub wall_utc: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    /// Raw file, relative to the session directory.
    pub file: String,
    pub offset: u64,
    pub len: u64,
    /// 1-based line number in the raw file.
    pub line: u64,
    /// The subscriber event sequence number, for `events` records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

/// A timeline entry together with the raw text it points at.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Record {
    #[serde(flatten)]
    pub entry: TimelineEntry,
    /// The raw record, rendered for reading.
    pub text: String,
}

/// Compact in-memory form used only while building.
struct Pending {
    mach_ns: u64,
    source: u32,
    file: u32,
    pid: Option<i64>,
    offset: u64,
    len: u64,
    line: u64,
    seq: Option<u64>,
}

#[derive(Default)]
struct Interner {
    names: Vec<String>,
    ids: HashMap<String, u32>,
}

impl Interner {
    fn id(&mut self, name: &str) -> u32 {
        if let Some(id) = self.ids.get(name) {
            return *id;
        }

        let id = self.names.len() as u32;
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildStats {
    pub entries: u64,
    pub by_source: BTreeMap<String, u64>,
}

fn wall_utc(clock: &ClockMap, mach_ns: u64) -> Option<String> {
    let wall_us = clock.mach_to_wall(mach_ns)?;
    format_wall_us(wall_us)
}

#[must_use]
pub fn format_wall_us(wall_us: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_micros(wall_us)
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
}

/// Which timeline source an unknown file under `oslog/` belongs to, from its name alone. Used for
/// hand-added files and when the manifest does not list a stream.
fn source_for_oslog_file(relative: &str) -> String {
    let name = relative.strip_prefix("oslog/").unwrap_or(relative);
    match name {
        "komorebi.ndjson" => "oslog:komorebi".to_string(),
        _ if name.starts_with("komorebi-") && name.ends_with(".ndjson") => {
            "oslog:komorebi".to_string()
        }
        "system.ndjson" => "oslog:system".to_string(),
        "all.ndjson" => "oslog:all".to_string(),
        _ => {
            let stem = name
                .strip_prefix("apps/")
                .unwrap_or(name)
                .trim_end_matches(".ndjson");
            let exe = stem.rsplit_once('-').map_or(stem, |(exe, _pid)| exe);
            format!("oslog:app:{exe}")
        }
    }
}

fn list_oslog_files(session: &Session) -> Vec<String> {
    let mut files = Vec::new();
    for sub in ["oslog", "oslog/apps"] {
        let Ok(entries) = std::fs::read_dir(session.path(sub)) else {
            continue;
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".ndjson") {
                files.push(format!("{sub}/{name}"));
            }
        }
    }

    files.sort();
    files
}

/// Build `timeline.jsonl` from the raw files and the manifest.
pub fn build(session: &Session) -> eyre::Result<BuildStats> {
    let manifest = session.manifest()?;
    let clock = ClockMap::new(manifest.anchors.clone());
    let mut sources = Interner::default();
    let mut files = Interner::default();
    let mut pending: Vec<Pending> = Vec::new();

    // komorebi's own log: UTC wall clock -> mach via the anchors
    let log_path = session.path("komorebi.log");
    if log_path.is_file() {
        let bytes = std::fs::read(&log_path)?;
        let source = sources.id(SOURCE_KOMOREBI_LOG);
        let file = files.id("komorebi.log");
        for line in komorebi_log::index_lines(&bytes) {
            let Some(timestamp) = line.timestamp else {
                continue;
            };
            let Some(mach_ns) = clock.wall_to_mach(timestamp.timestamp_micros()) else {
                continue;
            };

            pending.push(Pending {
                mach_ns,
                source,
                file,
                pid: None,
                offset: line.offset,
                len: line.len,
                line: line.number,
                seq: None,
            });
        }
    }

    // subscriber events: stamped on the mach clock at receipt
    let events_path = session.path("events.jsonl");
    if events_path.is_file() {
        let source = sources.id(SOURCE_EVENTS);
        let file = files.id("events.jsonl");
        let mut reader = BufReader::new(File::open(&events_path)?);
        let mut buffer = String::new();
        let (mut offset, mut number) = (0_u64, 0_u64);
        loop {
            buffer.clear();
            let read = reader.read_line(&mut buffer)?;
            if read == 0 {
                break;
            }

            number += 1;
            if let Ok(row) = serde_json::from_str::<EventRow>(&buffer) {
                pending.push(Pending {
                    mach_ns: row.recv_mach_ns,
                    source,
                    file,
                    pid: row
                        .event
                        .as_ref()
                        .and_then(managed_apps::event_pid)
                        .map(i64::from),
                    offset,
                    len: read as u64,
                    line: number,
                    seq: Some(row.seq),
                });
            }

            offset += read as u64;
        }
    }

    // incidents (display changes, sleep/wake, restarts): stamped on the mach clock when noticed
    let incidents_path = session.path(INCIDENTS_FILE);
    if incidents_path.is_file() {
        let source = sources.id(SOURCE_RECORDER);
        let file = files.id(INCIDENTS_FILE);
        let mut reader = BufReader::new(File::open(&incidents_path)?);
        let mut buffer = String::new();
        let (mut offset, mut number) = (0_u64, 0_u64);
        loop {
            buffer.clear();
            let read = reader.read_line(&mut buffer)?;
            if read == 0 {
                break;
            }

            number += 1;
            if let Ok(incident) = serde_json::from_str::<Incident>(&buffer) {
                pending.push(Pending {
                    mach_ns: incident.mach_ns,
                    source,
                    file,
                    pid: None,
                    offset,
                    len: read as u64,
                    line: number,
                    seq: None,
                });
            }

            offset += read as u64;
        }
    }

    // unified log streams: already on the mach clock
    let manifest_sources: HashMap<&str, &str> = manifest
        .oslog
        .iter()
        .map(|info| (info.spec.file.as_str(), info.spec.source.as_str()))
        .collect();

    for relative in list_oslog_files(session) {
        let source_name = manifest_sources.get(relative.as_str()).map_or_else(
            || source_for_oslog_file(&relative),
            |source| (*source).to_string(),
        );
        let source = sources.id(&source_name);
        let file = files.id(&relative);

        let mut reader = BufReader::with_capacity(256 * 1024, File::open(session.path(&relative))?);
        let mut buffer = Vec::new();
        let (mut offset, mut number) = (0_u64, 0_u64);
        loop {
            buffer.clear();
            let read = reader.read_until(b'\n', &mut buffer)?;
            if read == 0 {
                break;
            }

            number += 1;
            if let Some(ticks) = oslog::extract_mach_ticks(&buffer) {
                pending.push(Pending {
                    mach_ns: crate::clock::timebase().ticks_to_ns(ticks),
                    source,
                    file,
                    pid: extract_pid(&buffer),
                    offset,
                    len: read as u64,
                    line: number,
                    seq: None,
                });
            }

            offset += read as u64;
        }
    }

    pending.sort_by_key(|entry| (entry.mach_ns, entry.source, entry.file, entry.offset));

    let path = session.path(TIMELINE_FILE);
    let temporary = session.path(format!("{TIMELINE_FILE}.tmp"));
    let mut writer = BufWriter::new(
        File::create(&temporary)
            .wrap_err_with(|| format!("could not create {}", temporary.display()))?,
    );

    let mut stats = BuildStats::default();
    for entry in &pending {
        let source = &sources.names[entry.source as usize];
        let row = TimelineEntry {
            video_s: manifest.video_seconds(entry.mach_ns),
            mach_ns: entry.mach_ns,
            wall_utc: wall_utc(&clock, entry.mach_ns),
            source: source.clone(),
            pid: entry.pid,
            file: files.names[entry.file as usize].clone(),
            offset: entry.offset,
            len: entry.len,
            line: entry.line,
            seq: entry.seq,
        };

        writeln!(writer, "{}", serde_json::to_string(&row)?)?;
        stats.entries += 1;
        *stats.by_source.entry(source.clone()).or_default() += 1;
    }

    writer.flush()?;
    drop(writer);
    std::fs::rename(&temporary, &path)?;
    Ok(stats)
}

/// Pull `"processID":<digits>` out of a raw ndjson line.
fn extract_pid(line: &[u8]) -> Option<i64> {
    const KEY: &[u8] = b"\"processID\":";
    let start = line.windows(KEY.len()).position(|window| window == KEY)? + KEY.len();
    let digits: Vec<u8> = line[start..]
        .iter()
        .copied()
        .take_while(|byte| byte.is_ascii_digit() || *byte == b'-')
        .collect();

    std::str::from_utf8(&digits).ok()?.parse().ok()
}

/// Narrowing for queries.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// An app's exe name (case-insensitive) or a pid.
    pub app: Option<String>,
    /// Sources to keep: `komorebi-log`, `events`, `oslog` (all OS trace), `oslog:komorebi`,
    /// `oslog:system`, `oslog:app` (all apps) or `oslog:app:<exe>`. Empty keeps everything.
    pub sources: Vec<String>,
}

fn source_matches(source: &str, wanted: &str) -> bool {
    source.eq_ignore_ascii_case(wanted)
        || source.get(..wanted.len()).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case(wanted)
                && source.as_bytes().get(wanted.len()) == Some(&b':')
        })
}

/// The pids and source labels an `--app` argument refers to, using the manifest's app list.
fn resolve_app(manifest: &Manifest, app: &str) -> (Vec<i64>, String) {
    if let Ok(pid) = app.parse::<i64>() {
        return (vec![pid], String::new());
    }

    let pids = manifest
        .apps
        .iter()
        .filter(|record| record.exe.eq_ignore_ascii_case(app))
        .map(|record| i64::from(record.pid))
        .collect();

    (pids, format!("oslog:app:{app}"))
}

impl Filter {
    fn accepts(&self, entry: &TimelineEntry, app: Option<&(Vec<i64>, String)>) -> bool {
        if !self.sources.is_empty()
            && !self
                .sources
                .iter()
                .any(|wanted| source_matches(&entry.source, wanted))
        {
            return false;
        }

        if let Some((pids, source)) = app {
            let by_pid = entry.pid.is_some_and(|pid| pids.contains(&pid));
            let by_source = !source.is_empty() && entry.source.eq_ignore_ascii_case(source);
            // `--app` is "only this app's records": the komorebi log and events are not that
            // app's, even though events may mention its pid
            let is_app_data = entry.source.starts_with("oslog:");
            return is_app_data && (by_pid || by_source);
        }

        true
    }
}

fn read_entry_line(
    reader: &mut BufReader<File>,
    buffer: &mut String,
) -> eyre::Result<Option<(TimelineEntry, u64)>> {
    buffer.clear();
    let read = reader.read_line(buffer)?;
    if read == 0 {
        return Ok(None);
    }

    let entry: TimelineEntry = serde_json::from_str(buffer.trim_end())
        .wrap_err("timeline.jsonl contains an invalid line; run `reindex`")?;
    Ok(Some((entry, read as u64)))
}

/// The byte offset of the first line whose `mach_ns` is `>= target`, or the file length.
fn lower_bound(file: &mut BufReader<File>, size: u64, target: u64) -> eyre::Result<u64> {
    let mut buffer = String::new();

    // the first line start at or after `position`
    let line_start_at_or_after = |file: &mut BufReader<File>, position: u64| -> eyre::Result<u64> {
        if position == 0 {
            return Ok(0);
        }

        file.seek(SeekFrom::Start(position - 1))?;
        let mut skipped = Vec::new();
        let read = file.read_until(b'\n', &mut skipped)?;
        Ok(position - 1 + read as u64)
    };

    let (mut low, mut high) = (0_u64, size);
    while low < high {
        let middle = low + (high - low) / 2;
        let start = line_start_at_or_after(file, middle)?;
        if start >= high {
            high = middle;
            continue;
        }

        file.seek(SeekFrom::Start(start))?;
        match read_entry_line(file, &mut buffer)? {
            Some((entry, length)) => {
                if entry.mach_ns < target {
                    low = start + length;
                } else {
                    high = start;
                }
            }
            None => high = middle,
        }
    }

    Ok(low)
}

/// Raw files opened on demand while rendering records.
struct RawFiles<'a> {
    session: &'a Session,
    open: HashMap<String, File>,
}

impl<'a> RawFiles<'a> {
    fn new(session: &'a Session) -> Self {
        Self {
            session,
            open: HashMap::new(),
        }
    }

    fn read(&mut self, entry: &TimelineEntry) -> eyre::Result<String> {
        if !self.open.contains_key(&entry.file) {
            let path = self.session.path(&entry.file);
            let file =
                File::open(&path).wrap_err_with(|| format!("could not open {}", path.display()))?;
            self.open.insert(entry.file.clone(), file);
        }

        let file = self
            .open
            .get_mut(&entry.file)
            .ok_or_else(|| eyre::Report::msg("raw file vanished"))?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut bytes = vec![0_u8; entry.len as usize];
        file.read_exact(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes)
            .trim_end_matches(['\n', '\r'])
            .to_string())
    }
}

const EVENT_TEXT_LIMIT: usize = 400;

/// Render a raw record for reading.
#[must_use]
pub fn render(entry: &TimelineEntry, raw: &str) -> String {
    if entry.source == SOURCE_KOMOREBI_LOG {
        return strip_ansi(raw);
    }

    if entry.source == SOURCE_RECORDER {
        return match serde_json::from_str::<Incident>(raw) {
            Ok(incident) => format!("{}: {}", incident.kind, incident.detail),
            Err(_) => truncate(raw, EVENT_TEXT_LIMIT),
        };
    }

    if entry.source == SOURCE_EVENTS {
        return match serde_json::from_str::<EventRow>(raw) {
            Ok(row) => {
                let event = row
                    .event
                    .map_or_else(|| "<unparsed>".to_string(), |event| event.to_string());
                let event = truncate(&event, EVENT_TEXT_LIMIT);
                let state = row
                    .state_id
                    .map_or_else(String::new, |id| format!("  state=states/{id:06}.json"));
                format!("#{} {event}{state}", row.seq)
            }
            Err(_) => truncate(raw, EVENT_TEXT_LIMIT),
        };
    }

    match OsLogRecord::parse(raw) {
        Some(record) => {
            let time = record
                .timestamp
                .split_whitespace()
                .nth(1)
                .unwrap_or(&record.timestamp);
            let place = match (record.subsystem.is_empty(), record.category.is_empty()) {
                (true, _) => String::new(),
                (false, true) => format!(" {}", record.subsystem),
                (false, false) => format!(" {}/{}", record.subsystem, record.category),
            };
            format!(
                "{time} [{}] {}{place}: {}",
                record.pid,
                record.process_name(),
                compact_lines(&record.message)
            )
        }
        None => truncate(raw, EVENT_TEXT_LIMIT),
    }
}

/// Remove ANSI escape sequences. komorebi's plaintext log still contains them inside span fields
/// (tracing shares the formatted fields between its two layers); the raw file is left as-is and
/// only the rendering is cleaned.
#[must_use]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // a CSI sequence ends at the first byte in 0x40..=0x7e
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }

    out
}

/// Fold a multi-line message onto one line, marking the breaks.
fn compact_lines(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let mut out = lines.next().unwrap_or_default().to_string();
    for line in lines {
        out.push_str(" ⏎ ");
        out.push_str(line);
    }

    out
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }

    let mut shortened: String = text.chars().take(limit).collect();
    shortened.push('…');
    shortened
}

/// Every record whose video time is in `from_s..=to_s`, narrowed by `filter`.
pub fn query(
    session: &Session,
    manifest: &Manifest,
    from_s: f64,
    to_s: f64,
    filter: &Filter,
) -> eyre::Result<Vec<Record>> {
    let path = session.path(TIMELINE_FILE);
    let file = File::open(&path).wrap_err_with(|| {
        format!(
            "{} does not exist; run `komorebi-recorder reindex`",
            path.display()
        )
    })?;
    let size = file.metadata()?.len();
    let mut reader = BufReader::new(file);

    let from_mach = manifest.mach_at_video_seconds(from_s);
    let to_mach = manifest.mach_at_video_seconds(to_s);
    let start = lower_bound(&mut reader, size, from_mach)?;
    reader.seek(SeekFrom::Start(start))?;

    let app = filter.app.as_deref().map(|app| resolve_app(manifest, app));
    let mut raw = RawFiles::new(session);
    let mut records = Vec::new();
    let mut buffer = String::new();

    while let Some((entry, _)) = read_entry_line(&mut reader, &mut buffer)? {
        if entry.mach_ns > to_mach {
            break;
        }

        if !filter.accepts(&entry, app.as_ref()) {
            continue;
        }

        let text = render(&entry, &raw.read(&entry)?);
        records.push(Record { entry, text });
    }

    Ok(records)
}

/// The distinct application names that have OS trace streams in this session.
#[must_use]
pub fn app_names(manifest: &Manifest) -> Vec<String> {
    let mut names: Vec<String> = manifest.apps.iter().map(|app| app.exe.clone()).collect();
    names.sort();
    names.dedup();
    names
}

/// A per-bucket digest of the whole recording.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bucket {
    pub start_s: f64,
    pub end_s: f64,
    pub counts: BTreeMap<String, u64>,
    pub first_log: Option<String>,
    pub last_log: Option<String>,
}

pub fn summary(
    session: &Session,
    manifest: &Manifest,
    bucket_s: f64,
    filter: &Filter,
) -> eyre::Result<Vec<Bucket>> {
    let bucket_s = bucket_s.max(0.001);
    let path = session.path(TIMELINE_FILE);
    let mut reader = BufReader::new(File::open(&path).wrap_err_with(|| {
        format!(
            "{} does not exist; run `komorebi-recorder reindex`",
            path.display()
        )
    })?);

    let app = filter.app.as_deref().map(|app| resolve_app(manifest, app));
    let mut raw = RawFiles::new(session);
    let mut buckets: BTreeMap<i64, Bucket> = BTreeMap::new();
    let mut buffer = String::new();

    while let Some((entry, _)) = read_entry_line(&mut reader, &mut buffer)? {
        if !filter.accepts(&entry, app.as_ref()) {
            continue;
        }

        let index = (entry.video_s / bucket_s).floor() as i64;
        let bucket = buckets.entry(index).or_insert_with(|| Bucket {
            start_s: index as f64 * bucket_s,
            end_s: (index + 1) as f64 * bucket_s,
            counts: BTreeMap::new(),
            first_log: None,
            last_log: None,
        });

        *bucket.counts.entry(entry.source.clone()).or_default() += 1;

        if entry.source == SOURCE_KOMOREBI_LOG {
            let line = truncate(&raw.read(&entry)?, 160);
            if bucket.first_log.is_none() {
                bucket.first_log = Some(line.clone());
            }
            bucket.last_log = Some(line);
        }
    }

    Ok(buckets.into_values().collect())
}

/// What to look up in reverse.
#[derive(Debug, Clone)]
pub enum Locator {
    Utc(DateTime<Utc>),
    LogLine(u64),
    EventSeq(u64),
}

/// A record located in video time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Located {
    pub description: String,
    pub video_s: f64,
    pub mach_ns: u64,
    pub wall_utc: Option<String>,
    pub in_video: bool,
    /// Where it falls in the file of each display that has video at that moment.
    pub displays: Vec<DisplayTime>,
    /// Displays that were recorded at some point but have no video at this moment (unplugged,
    /// asleep, or a stream that had died).
    pub without_video: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DisplayTime {
    pub display: u32,
    pub segment: u32,
    pub file: String,
    pub file_s: f64,
}

pub fn locate(session: &Session, manifest: &Manifest, locator: &Locator) -> eyre::Result<Located> {
    let clock = ClockMap::new(manifest.anchors.clone());

    let (description, mach_ns) = match locator {
        Locator::Utc(time) => {
            let mach = clock
                .wall_to_mach(time.timestamp_micros())
                .ok_or_else(|| eyre::Report::msg("the session has no clock anchors"))?;
            (
                format!("UTC {}", time.format("%Y-%m-%dT%H:%M:%S%.6fZ")),
                mach,
            )
        }
        Locator::LogLine(number) => {
            let bytes = std::fs::read(session.path("komorebi.log"))
                .wrap_err("could not read komorebi.log")?;
            let lines = komorebi_log::index_lines(&bytes);
            let line = lines
                .get(usize::try_from(number.saturating_sub(1))?)
                .filter(|_| *number >= 1)
                .ok_or_else(|| {
                    eyre::Report::msg(format!(
                        "komorebi.log has {} lines; there is no line {number}",
                        lines.len()
                    ))
                })?;
            let timestamp = line.timestamp.ok_or_else(|| {
                eyre::Report::msg(format!(
                    "line {number} has no timestamp and none precedes it"
                ))
            })?;
            let mach = clock
                .wall_to_mach(timestamp.timestamp_micros())
                .ok_or_else(|| eyre::Report::msg("the session has no clock anchors"))?;
            let text = &bytes[line.offset as usize..(line.offset + line.len) as usize];
            (
                format!(
                    "komorebi.log line {number}: {}",
                    truncate(String::from_utf8_lossy(text).trim_end(), 120)
                ),
                mach,
            )
        }
        Locator::EventSeq(seq) => {
            let text = std::fs::read_to_string(session.path("events.jsonl"))
                .wrap_err("could not read events.jsonl")?;
            let row = text
                .lines()
                .filter_map(|line| serde_json::from_str::<EventRow>(line).ok())
                .find(|row| row.seq == *seq)
                .ok_or_else(|| eyre::Report::msg(format!("there is no event #{seq}")))?;
            let summary = row
                .event
                .as_ref()
                .map_or_else(String::new, |event| truncate(&event.to_string(), 120));
            (format!("event #{seq}: {summary}"), row.recv_mach_ns)
        }
    };

    let video_s = manifest.video_seconds(mach_ns);
    let (_, duration) = manifest.video_span();

    // for each display, the segment (file) that has video at this moment, or why there is none
    let mut displays = Vec::new();
    let mut without_video = Vec::new();
    for id in manifest.display_ids() {
        match manifest.segment_at(id, video_s) {
            Some(entry) => displays.push(DisplayTime {
                display: id,
                segment: entry.result.segment,
                file: entry.result.file.clone(),
                file_s: video_s - entry.offset_s,
            }),
            None => without_video.push(id),
        }
    }

    Ok(Located {
        description,
        video_s,
        mach_ns,
        wall_utc: wall_utc(&clock, mach_ns),
        in_video: (0.0..=duration).contains(&video_s),
        displays,
        without_video,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Anchor;
    use crate::session::Moment;
    use crate::sources::oslog::OsLogLevel;

    /// A synthetic but realistic session: 10s of video starting at mach 100s.
    struct Fixture {
        session: Session,
        manifest: Manifest,
    }

    const T0_MACH_NS: u64 = 100_000_000_000;
    /// wall = mach + this offset (microseconds)
    const WALL_OFFSET_US: i64 = 1_800_000_000_000_000 - 100_000_000;

    fn wall_for_video_s(video_s: f64) -> i64 {
        (T0_MACH_NS as i64 / 1_000) + (video_s * 1e6) as i64 + WALL_OFFSET_US
    }

    fn rfc3339(wall_us: i64) -> String {
        format_wall_us(wall_us).expect("time")
    }

    fn oslog_line(video_s: f64, pid: i64, process: &str, message: &str) -> String {
        let tb = crate::clock::timebase();
        let ticks = tb.ns_to_ticks(T0_MACH_NS + (video_s * 1e9) as u64);
        format!(
            r#"{{"machTimestamp":{ticks},"timestamp":"2026-10-26 12:00:00.000000-0700","processID":{pid},"processImagePath":"/Applications/{process}","subsystem":"com.apple.accessibility.axserver","category":"AX","messageType":"Debug","eventMessage":"{message}"}}"#
        )
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-timeline-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let session = Session::create(Some(dir), Utc::now()).expect("create");

        let start = Moment {
            mach_ns: T0_MACH_NS - 1_000_000_000,
            wall_us: wall_for_video_s(-1.0),
        };
        let end = Moment {
            mach_ns: T0_MACH_NS + 11_000_000_000,
            wall_us: wall_for_video_s(11.0),
        };
        let mut manifest = Manifest::new(
            start,
            vec![
                Anchor {
                    wall_us: start.wall_us,
                    mach_ns: start.mach_ns,
                },
                Anchor {
                    wall_us: wall_for_video_s(5.0),
                    mach_ns: T0_MACH_NS + 5_000_000_000,
                },
                Anchor {
                    wall_us: end.wall_us,
                    mach_ns: end.mach_ns,
                },
            ],
        );
        manifest.stop = Some(end);
        manifest.video_t0_mach_ns = Some(T0_MACH_NS);
        manifest.video_end_mach_ns = Some(T0_MACH_NS + 10_000_000_000);

        // komorebi.log: one line per second at 2s, 3s and 4s, with a 2-line panic after 3s
        let log = format!(
            "{} INFO komorebi::process_event: processing event: Show at 2s\n\
             {} INFO komorebi::process_event: processing event: FocusChange at 3s\n\
             thread 'main' panicked at src/x.rs:1:1:\n\
             \x20  0: backtrace frame\n\
             {} DEBUG komorebi::window_manager: at 4s\n",
            rfc3339(wall_for_video_s(2.0)),
            rfc3339(wall_for_video_s(3.0)),
            rfc3339(wall_for_video_s(4.0)),
        );
        std::fs::write(session.path("komorebi.log"), log).expect("log");

        // events at 2.01s and 3.01s
        let event = |seq: u64, video_s: f64, kind: &str, pid: i64| {
            serde_json::json!({
                "seq": seq,
                "recv_wall_us": wall_for_video_s(video_s),
                "recv_mach_ns": T0_MACH_NS + (video_s * 1e9) as u64,
                "event": {"type": kind, "content": [{"type": "Accessibility", "content": "X"}, pid, 7]},
                "state_id": 1,
            })
            .to_string()
        };
        std::fs::write(
            session.path("events.jsonl"),
            format!(
                "{}\n{}\n",
                event(1, 2.01, "Show", 111),
                event(2, 3.01, "FocusChange", 222)
            ),
        )
        .expect("events");

        // oslog: komorebi, system, and two apps
        let write_stream = |file: &str, lines: Vec<String>| {
            std::fs::write(session.path(file), lines.join("\n") + "\n").expect("stream");
        };
        write_stream(
            "oslog/komorebi.ndjson",
            vec![
                oslog_line(2.02, 999, "komorebi", "ax call"),
                oslog_line(8.0, 999, "komorebi", "late"),
            ],
        );
        write_stream(
            "oslog/system.ndjson",
            vec![oslog_line(2.5, 50, "axserver", "serve")],
        );
        write_stream(
            "oslog/apps/Ghostty-111.ndjson",
            vec![
                oslog_line(2.03, 111, "Ghostty", "tab changed"),
                oslog_line(3.02, 111, "Ghostty", "tab changed again"),
            ],
        );
        write_stream(
            "oslog/apps/Brave_Beta-222.ndjson",
            vec![oslog_line(3.03, 222, "Brave", "other app")],
        );

        let spec = |name: &str, source: &str, file: &str, pid: Option<i32>| oslog::StreamInfo {
            spec: oslog::StreamSpec {
                name: name.into(),
                source: source.into(),
                predicate: "x".into(),
                level: OsLogLevel::Debug,
                file: file.into(),
                pid,
            },
            spawned_mach_ns: 0,
            ready_mach_ns: None,
            header: None,
            events: 1,
            bytes: 1,
            first_event_mach_ns: None,
            last_event_mach_ns: None,
            restarts: 0,
            error: None,
        };
        manifest.oslog = vec![
            spec(
                "komorebi",
                "oslog:komorebi",
                "oslog/komorebi.ndjson",
                Some(999),
            ),
            spec("system", "oslog:system", "oslog/system.ndjson", None),
            spec(
                "app:Ghostty-111",
                "oslog:app:Ghostty",
                "oslog/apps/Ghostty-111.ndjson",
                Some(111),
            ),
            spec(
                "app:Brave Beta-222",
                "oslog:app:Brave Beta",
                "oslog/apps/Brave_Beta-222.ndjson",
                Some(222),
            ),
        ];
        manifest.apps = vec![
            managed_apps::AppRecord {
                pid: 111,
                exe: "Ghostty".into(),
                window_ids: vec![1],
                first_seen_mach_ns: 0,
                origin: "state".into(),
                ..managed_apps::AppRecord::default()
            },
            managed_apps::AppRecord {
                pid: 222,
                exe: "Brave Beta".into(),
                window_ids: vec![2],
                first_seen_mach_ns: 0,
                origin: "state".into(),
                ..managed_apps::AppRecord::default()
            },
        ];

        session.write_manifest(&manifest).expect("manifest");
        build(&session).expect("build");
        Fixture { session, manifest }
    }

    fn texts(records: &[Record]) -> Vec<String> {
        records.iter().map(|record| record.text.clone()).collect()
    }

    #[test]
    fn the_timeline_is_sorted_and_complete() {
        let fixture = fixture("sorted");
        let text = std::fs::read_to_string(fixture.session.path(TIMELINE_FILE)).expect("timeline");
        let entries: Vec<TimelineEntry> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("entry"))
            .collect();

        // 5 log lines (the 2 continuation lines inherit) + 2 events + 2 + 1 + 2 + 1 oslog
        assert_eq!(entries.len(), 5 + 2 + 6);
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].mach_ns <= pair[1].mach_ns)
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.source == SOURCE_KOMOREBI_LOG)
                .count(),
            5
        );
        assert!(entries.iter().any(|e| e.source == "oslog:app:Brave Beta"));
    }

    #[test]
    fn at_returns_every_source_within_the_window() {
        let fixture = fixture("at");
        let records = query(
            &fixture.session,
            &fixture.manifest,
            1.5,
            3.5,
            &Filter::default(),
        )
        .expect("query");
        let sources: std::collections::BTreeSet<&str> =
            records.iter().map(|r| r.entry.source.as_str()).collect();

        for source in [
            SOURCE_KOMOREBI_LOG,
            SOURCE_EVENTS,
            "oslog:komorebi",
            "oslog:system",
            "oslog:app:Ghostty",
        ] {
            assert!(sources.contains(source), "missing {source} in {sources:?}");
        }

        // outside the window: the 8s komorebi line and the 4s log line
        let all = texts(&records).join("\n");
        assert!(!all.contains("late"));
        assert!(!all.contains("at 4s"));
        assert!(all.contains("processing event: Show at 2s"));
    }

    #[test]
    fn continuation_lines_share_the_previous_time_and_stay_in_the_window() {
        let fixture = fixture("continuation");
        let records = query(
            &fixture.session,
            &fixture.manifest,
            2.9,
            3.1,
            &Filter {
                sources: vec![SOURCE_KOMOREBI_LOG.into()],
                app: None,
            },
        )
        .expect("query");

        let lines = texts(&records);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].contains("panicked"));
        assert!(lines[2].contains("backtrace frame"));
    }

    #[test]
    fn app_filter_returns_only_that_apps_records() {
        let fixture = fixture("app");
        let filter = Filter {
            app: Some("ghostty".into()),
            sources: vec![],
        };
        let records =
            query(&fixture.session, &fixture.manifest, 0.0, 10.0, &filter).expect("query");

        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|r| r.entry.source == "oslog:app:Ghostty" && r.entry.pid == Some(111))
        );
        assert!(texts(&records).iter().all(|text| text.contains("Ghostty")));

        // by pid reaches the same records, and never the komorebi log or events that mention it
        let by_pid = query(
            &fixture.session,
            &fixture.manifest,
            0.0,
            10.0,
            &Filter {
                app: Some("111".into()),
                sources: vec![],
            },
        )
        .expect("query");
        assert_eq!(by_pid.len(), 2);
    }

    #[test]
    fn source_filter_supports_prefixes() {
        let fixture = fixture("source");
        let run = |sources: &[&str]| {
            query(
                &fixture.session,
                &fixture.manifest,
                0.0,
                10.0,
                &Filter {
                    app: None,
                    sources: sources.iter().map(|s| (*s).to_string()).collect(),
                },
            )
            .expect("query")
            .len()
        };

        assert_eq!(run(&["events"]), 2);
        assert_eq!(run(&["oslog"]), 6);
        assert_eq!(run(&["oslog:app"]), 3);
        assert_eq!(run(&["oslog:app:Ghostty"]), 2);
        assert_eq!(run(&["oslog:komorebi", "events"]), 4);
        assert_eq!(run(&["osl"]), 0, "a source prefix must end at a colon");
    }

    #[test]
    fn range_endpoints_are_inclusive_and_binary_search_is_exact() {
        let fixture = fixture("range");
        let run = |from: f64, to: f64| {
            query(
                &fixture.session,
                &fixture.manifest,
                from,
                to,
                &Filter::default(),
            )
            .expect("query")
            .len()
        };

        assert_eq!(run(-100.0, 100.0), 13);
        assert_eq!(run(100.0, 200.0), 0);
        assert_eq!(run(-10.0, -5.0), 0);
        assert_eq!(
            run(8.0, 8.0),
            1,
            "an instant selects the record at exactly that time"
        );
        // the first record of the whole timeline is at 2.0s
        assert_eq!(run(2.0, 2.0), 1);
        assert_eq!(run(1.99, 2.005), 1);
    }

    #[test]
    fn the_wall_clock_log_maps_through_the_anchors() {
        let fixture = fixture("anchors");
        let records = query(
            &fixture.session,
            &fixture.manifest,
            0.0,
            10.0,
            &Filter {
                sources: vec![SOURCE_KOMOREBI_LOG.into()],
                app: None,
            },
        )
        .expect("query");

        let show = records
            .iter()
            .find(|r| r.text.contains("Show at 2s"))
            .expect("show");
        assert!(
            (show.entry.video_s - 2.0).abs() < 1e-3,
            "{}",
            show.entry.video_s
        );
        let focus = records
            .iter()
            .find(|r| r.text.contains("FocusChange at 3s"))
            .expect("focus");
        assert!(
            (focus.entry.video_s - 3.0).abs() < 1e-3,
            "{}",
            focus.entry.video_s
        );
    }

    #[test]
    fn locate_inverts_the_queries() {
        let fixture = fixture("locate");

        let by_line =
            locate(&fixture.session, &fixture.manifest, &Locator::LogLine(2)).expect("line");
        assert!((by_line.video_s - 3.0).abs() < 1e-3);
        assert!(by_line.in_video);
        assert!(by_line.description.contains("FocusChange at 3s"));

        // a continuation line has the time of the line it follows
        let continuation =
            locate(&fixture.session, &fixture.manifest, &Locator::LogLine(4)).expect("line");
        assert!((continuation.video_s - 3.0).abs() < 1e-3);

        let by_event =
            locate(&fixture.session, &fixture.manifest, &Locator::EventSeq(1)).expect("event");
        assert!((by_event.video_s - 2.01).abs() < 1e-6);

        let utc: DateTime<Utc> = rfc3339(wall_for_video_s(6.5)).parse().expect("utc");
        let by_utc = locate(&fixture.session, &fixture.manifest, &Locator::Utc(utc)).expect("utc");
        assert!((by_utc.video_s - 6.5).abs() < 1e-3);

        let outside: DateTime<Utc> = rfc3339(wall_for_video_s(60.0)).parse().expect("utc");
        assert!(
            !locate(&fixture.session, &fixture.manifest, &Locator::Utc(outside))
                .expect("utc")
                .in_video
        );

        assert!(locate(&fixture.session, &fixture.manifest, &Locator::LogLine(99)).is_err());
        assert!(locate(&fixture.session, &fixture.manifest, &Locator::LogLine(0)).is_err());
        assert!(locate(&fixture.session, &fixture.manifest, &Locator::EventSeq(99)).is_err());
    }

    #[test]
    fn at_and_locate_round_trip() {
        let fixture = fixture("roundtrip");
        // pick every komorebi log line, locate it, then query at that time: it must come back
        for number in [1_u64, 2, 5] {
            let located = locate(
                &fixture.session,
                &fixture.manifest,
                &Locator::LogLine(number),
            )
            .expect("locate");
            let records = query(
                &fixture.session,
                &fixture.manifest,
                located.video_s - 0.001,
                located.video_s + 0.001,
                &Filter {
                    sources: vec![SOURCE_KOMOREBI_LOG.into()],
                    app: None,
                },
            )
            .expect("query");
            assert!(
                records.iter().any(|record| record.entry.line == number),
                "line {number} not found at {}s",
                located.video_s
            );
        }
    }

    #[test]
    fn summary_buckets_every_second() {
        let fixture = fixture("summary");
        let buckets =
            summary(&fixture.session, &fixture.manifest, 1.0, &Filter::default()).expect("summary");

        let at = |start: f64| {
            buckets
                .iter()
                .find(|b| (b.start_s - start).abs() < 1e-9)
                .expect("bucket")
        };
        assert_eq!(at(2.0).counts.get(SOURCE_KOMOREBI_LOG), Some(&1));
        assert_eq!(at(2.0).counts.get(SOURCE_EVENTS), Some(&1));
        assert!(
            at(2.0)
                .first_log
                .as_deref()
                .is_some_and(|line| line.contains("Show at 2s"))
        );
        // the 3s bucket holds the FocusChange line and both panic lines
        assert_eq!(at(3.0).counts.get(SOURCE_KOMOREBI_LOG), Some(&3));
        assert!(
            at(3.0)
                .last_log
                .as_deref()
                .is_some_and(|line| line.contains("backtrace frame"))
        );

        let total: u64 = buckets.iter().flat_map(|b| b.counts.values()).sum();
        assert_eq!(total, 13);
    }

    #[test]
    fn reindex_is_idempotent_and_recovers_a_deleted_timeline() {
        let fixture = fixture("reindex");
        let first = std::fs::read_to_string(fixture.session.path(TIMELINE_FILE)).expect("timeline");
        std::fs::remove_file(fixture.session.path(TIMELINE_FILE)).expect("remove");
        build(&fixture.session).expect("rebuild");
        let second =
            std::fs::read_to_string(fixture.session.path(TIMELINE_FILE)).expect("timeline");
        assert_eq!(first, second);
    }

    #[test]
    fn hand_added_oslog_files_are_indexed_by_name() {
        let fixture = fixture("handadded");
        std::fs::write(
            fixture.session.path("oslog/apps/Finder-333.ndjson"),
            oslog_line(5.0, 333, "Finder", "hand added") + "\n",
        )
        .expect("write");
        let stats = build(&fixture.session).expect("rebuild");
        assert_eq!(stats.by_source.get("oslog:app:Finder"), Some(&1));
    }

    #[test]
    fn render_formats_each_source() {
        let fixture = fixture("render");
        let records = query(
            &fixture.session,
            &fixture.manifest,
            2.0,
            2.1,
            &Filter::default(),
        )
        .expect("query");

        let event = records
            .iter()
            .find(|r| r.entry.source == SOURCE_EVENTS)
            .expect("event");
        assert!(event.text.starts_with("#1 {"), "{}", event.text);
        assert!(event.text.contains("states/000001.json"));

        let os = records
            .iter()
            .find(|r| r.entry.source == "oslog:komorebi")
            .expect("os");
        assert!(
            os.text
                .contains("[999] komorebi com.apple.accessibility.axserver/AX: ax call"),
            "{}",
            os.text
        );
    }

    #[test]
    fn rendering_cleans_ansi_and_folds_multiline_messages() {
        assert_eq!(
            strip_ansi("INFO focus_container{\u{1b}[3midx\u{1b}[0m\u{1b}[2m=\u{1b}[0m2}: x"),
            "INFO focus_container{idx=2}: x"
        );
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("trailing \u{1b}"), "trailing \u{1b}");
        assert_eq!(
            compact_lines("Resource lookup\n\tRequest : a\n\tResult : None"),
            "Resource lookup ⏎ Request : a ⏎ Result : None"
        );
        assert_eq!(compact_lines(""), "");
    }

    #[test]
    fn a_missing_timeline_explains_how_to_recover() {
        let fixture = fixture("missing");
        std::fs::remove_file(fixture.session.path(TIMELINE_FILE)).expect("remove");
        let error = query(
            &fixture.session,
            &fixture.manifest,
            0.0,
            1.0,
            &Filter::default(),
        )
        .expect_err("no timeline")
        .to_string();
        assert!(error.contains("reindex"), "{error}");
    }

    #[test]
    fn locate_names_the_segment_per_display_and_notes_a_display_with_no_video() {
        let fixture = fixture("segments");
        let mut manifest = fixture.manifest.clone();

        let segment = |display: u32, number: u32, t0_s: f64, end_s: f64| {
            let mut result = crate::capture::DisplayResult {
                display: crate::capture::DisplayDescriptor {
                    id: display,
                    frame: [0.0, 0.0, 10.0, 10.0],
                    width_points: 10,
                    height_points: 10,
                    asleep: false,
                },
                segment: number,
                scale: 1.0,
                width_px: 10,
                height_px: 10,
                codec: crate::capture::Codec::Hevc,
                file: crate::capture::video_file_name(display, number),
                frames_file: crate::capture::frames_file_name(display, number),
                video_t0_mach_ns: Some(T0_MACH_NS + (t0_s * 1e9) as u64),
                video_end_mach_ns: Some(T0_MACH_NS + (end_s * 1e9) as u64),
                end_reason: Some(crate::capture::EndReason::Stopped),
                frames: 5,
                dropped: 0,
                idle_callbacks: 0,
                latency: crate::capture::LatencyStats::default(),
                error: None,
            };
            result.frames = 5;
            result
        };

        // display 1: 0-4s, unplugged, back 8-10s; display 2: the whole time
        manifest.apply_segments(vec![
            segment(1, 1, 0.0, 4.0),
            segment(2, 1, 0.0, 10.0),
            segment(1, 2, 8.0, 10.0),
        ]);
        manifest.video_end_mach_ns = Some(T0_MACH_NS + 10_000_000_000);

        // log line 2 is at 3.0s: both displays have video
        let located = locate(&fixture.session, &manifest, &Locator::LogLine(2)).expect("locate");
        assert!(located.without_video.is_empty());
        let one = located
            .displays
            .iter()
            .find(|d| d.display == 1)
            .expect("display 1");
        assert_eq!((one.segment, one.file.as_str()), (1, "video-1.mp4"));

        // event 2 is at 3.01s too; pick a moment in the gap with a UTC time at 6s
        let utc: DateTime<Utc> = rfc3339(wall_for_video_s(6.0)).parse().expect("utc");
        let gap = locate(&fixture.session, &manifest, &Locator::Utc(utc)).expect("locate");
        assert_eq!(gap.without_video, vec![1], "display 1 was unplugged at 6s");
        assert_eq!(gap.displays.len(), 1);
        assert_eq!(gap.displays[0].display, 2);

        // after display 1 returns, the second segment is named and the time is within *its* file
        let utc: DateTime<Utc> = rfc3339(wall_for_video_s(9.0)).parse().expect("utc");
        let back = locate(&fixture.session, &manifest, &Locator::Utc(utc)).expect("locate");
        let one = back
            .displays
            .iter()
            .find(|d| d.display == 1)
            .expect("display 1");
        assert_eq!((one.segment, one.file.as_str()), (2, "video-1-2.mp4"));
        assert!(
            (one.file_s - 1.0).abs() < 1e-3,
            "9s is 1s into a segment that began at 8s: {}",
            one.file_s
        );
    }

    #[test]
    fn incidents_appear_in_the_timeline_next_to_everything_else() {
        let fixture = fixture("incidents");
        std::fs::write(
            fixture.session.path(INCIDENTS_FILE),
            format!(
                "{}\n",
                serde_json::json!({
                    "mach_ns": T0_MACH_NS + 2_500_000_000_u64,
                    "wall_us": wall_for_video_s(2.5),
                    "kind": "display-disconnected",
                    "detail": "display 1 disconnected",
                    "display": 1,
                })
            ),
        )
        .expect("write");
        build(&fixture.session).expect("build");

        let records = query(
            &fixture.session,
            &fixture.manifest,
            2.0,
            3.0,
            &Filter {
                app: None,
                sources: vec![SOURCE_RECORDER.into()],
            },
        )
        .expect("query");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].text,
            "display-disconnected: display 1 disconnected"
        );

        // and in an unfiltered `at`, with the komorebi log around it
        let all = query(
            &fixture.session,
            &fixture.manifest,
            2.0,
            3.0,
            &Filter::default(),
        )
        .expect("query");
        assert!(all.iter().any(|r| r.entry.source == SOURCE_RECORDER));
        assert!(all.iter().any(|r| r.entry.source == SOURCE_KOMOREBI_LOG));
    }
}

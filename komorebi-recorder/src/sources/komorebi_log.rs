//! Slicing komorebi's own `tracing` log for the duration of a recording.
//!
//! komorebi writes `komorebi_plaintext.log.<YYYY-MM-DD>` into its data directory, rolled daily at
//! **UTC** midnight, so a recording can span two files. Despite the name the text is not entirely
//! free of ANSI escape codes: they remain inside the fields of a span (`focus_window{idx=0}`),
//! so readers strip them before parsing. Line prefixes are
//! RFC 3339 UTC timestamps with microsecond precision. We snapshot a byte offset when recording
//! starts and the end offset when it stops, and copy exactly that byte range so multi-line
//! panics and backtraces (which carry no timestamp of their own) are preserved.

use chrono::DateTime;
use chrono::Duration;
use chrono::NaiveDate;
use chrono::Utc;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

pub const PLAINTEXT_PREFIX: &str = "komorebi_plaintext.log";

/// The file name of the log that tracing's daily roller writes for a given UTC date.
#[must_use]
pub fn log_file_name(date: NaiveDate) -> String {
    format!("{PLAINTEXT_PREFIX}.{}", date.format("%Y-%m-%d"))
}

/// Where in komorebi's log we were when the recording started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStart {
    pub date: NaiveDate,
    pub offset: u64,
}

/// One contiguous byte range of one rolled log file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogSegment {
    /// File name within komorebi's data directory.
    pub file: String,
    pub start_offset: u64,
    pub end_offset: u64,
    /// Where this segment begins inside the session's `komorebi.log`.
    pub out_offset: u64,
}

impl LogSegment {
    #[must_use]
    pub fn len(&self) -> u64 {
        self.end_offset.saturating_sub(self.start_offset)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// Record the current end of today's (UTC) log file.
#[must_use]
pub fn snapshot_start(dir: &Path, now: DateTime<Utc>) -> LogStart {
    let date = now.date_naive();
    LogStart {
        date,
        offset: file_len(&dir.join(log_file_name(date))),
    }
}

/// Work out which byte ranges of which files cover `start..=end_now`, including every whole file
/// for any UTC dates in between.
#[must_use]
pub fn plan_segments(dir: &Path, start: &LogStart, end_now: DateTime<Utc>) -> Vec<LogSegment> {
    let end_date = end_now.date_naive().max(start.date);
    let mut segments = Vec::new();
    let mut date = start.date;
    let mut out_offset = 0;

    while date <= end_date {
        let file = log_file_name(date);
        let path = dir.join(&file);
        let start_offset = if date == start.date { start.offset } else { 0 };
        let end_offset = file_len(&path);

        // a file that does not exist (komorebi was not logging that day) contributes nothing
        if path.exists() && end_offset > start_offset {
            let segment = LogSegment {
                file,
                start_offset,
                end_offset,
                out_offset,
            };
            out_offset += segment.len();
            segments.push(segment);
        }

        date += Duration::days(1);
    }

    segments
}

/// Copies the recording's slice of komorebi's log into the session's `komorebi.log` as it grows,
/// instead of in one go at the end, so a crash loses at most the last moment of it.
///
/// Rolled files are followed in date order. Once a later day's file has begun to be copied,
/// earlier files are frozen: a straggling late write to yesterday's file cannot be appended
/// without scrambling the order of the slice, and it would be a handful of lines at most.
pub struct LogTailer {
    dir: PathBuf,
    start: LogStart,
    out: File,
    out_len: u64,
    segments: Vec<LogSegment>,
}

impl LogTailer {
    pub fn create(dir: &Path, start: LogStart, out: &Path) -> eyre::Result<Self> {
        Ok(Self {
            dir: dir.to_path_buf(),
            start,
            out: File::create(out)
                .wrap_err_with(|| format!("could not create {}", out.display()))?,
            out_len: 0,
            segments: Vec::new(),
        })
    }

    /// Copy whatever komorebi has written since the last sync. Returns the bytes appended.
    pub fn sync(&mut self, now: DateTime<Utc>) -> eyre::Result<u64> {
        let planned = plan_segments(&self.dir, &self.start, now);
        let mut appended = 0;

        for plan in planned {
            let index = match self.segments.iter().position(|seg| seg.file == plan.file) {
                Some(index) => index,
                None => {
                    self.segments.push(LogSegment {
                        file: plan.file.clone(),
                        start_offset: plan.start_offset,
                        end_offset: plan.start_offset,
                        out_offset: self.out_len,
                    });
                    self.segments.len() - 1
                }
            };

            // a later file is already being followed: this one is frozen
            if index + 1 < self.segments.len() {
                continue;
            }

            let copied_to = self.segments[index].end_offset;
            if plan.end_offset <= copied_to {
                continue;
            }

            let path = self.dir.join(&plan.file);
            let mut file =
                File::open(&path).wrap_err_with(|| format!("could not open {}", path.display()))?;
            file.seek(SeekFrom::Start(copied_to))?;
            let copied = std::io::copy(&mut file.take(plan.end_offset - copied_to), &mut self.out)?;

            self.out.flush()?;
            self.segments[index].end_offset = copied_to + copied;
            self.out_len += copied;
            appended += copied;
        }

        Ok(appended)
    }

    /// What has been copied so far, as manifest segments.
    #[must_use]
    pub fn segments(&self) -> &[LogSegment] {
        &self.segments
    }
}

/// Parse the leading RFC 3339 token of a log line.
#[must_use]
pub fn parse_timestamp(line: &str) -> Option<DateTime<Utc>> {
    let token = line.split_whitespace().next()?;
    // cheap reject before the real parser: `YYYY-MM-DDT...`
    if token.len() < 20
        || token.as_bytes().get(4) != Some(&b'-')
        || token.as_bytes().get(10) != Some(&b'T')
    {
        return None;
    }

    DateTime::parse_from_rfc3339(token)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

/// The level token of a `tracing_subscriber::fmt` line (`INFO`, `DEBUG`, ...).
#[must_use]
pub fn parse_level(line: &str) -> Option<&str> {
    let level = line.split_whitespace().nth(1)?;
    matches!(level, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE").then_some(level)
}

/// A single line of the slice with its place in the file and its (possibly inherited) time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// 1-based line number within the slice.
    pub number: u64,
    pub offset: u64,
    pub len: u64,
    /// The line's own timestamp, or the previous timestamped line's for continuation lines.
    pub timestamp: Option<DateTime<Utc>>,
    pub is_continuation: bool,
}

/// Split a slice into lines, giving continuation lines (panic messages, backtraces) the timestamp
/// of the line they follow.
#[must_use]
pub fn index_lines(slice: &[u8]) -> Vec<LogLine> {
    let mut lines = Vec::new();
    let mut offset = 0_usize;
    let mut current: Option<DateTime<Utc>> = None;

    for (index, raw) in slice.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let number = index as u64 + 1;
        let text = String::from_utf8_lossy(raw);
        let own = parse_timestamp(&text);
        let is_continuation = own.is_none();
        if own.is_some() {
            current = own;
        }

        lines.push(LogLine {
            number,
            offset: offset as u64,
            len: raw.len() as u64,
            timestamp: current,
            is_continuation,
        });

        offset += raw.len();
    }

    lines
}

/// The distinct levels that appear in a slice of log text.
#[must_use]
pub fn observed_levels(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|line| parse_timestamp(line).is_some())
        .filter_map(parse_level)
        .map(str::to_string)
        .collect()
}

/// How komorebi's log verbosity was determined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedLevel {
    pub level: String,
    pub source: String,
}

/// Work out komorebi's log level from its command line and environment, mirroring the logic in
/// `komorebi/src/main.rs`: `RUST_LOG` wins if it was already set, otherwise `--log-level`, which
/// defaults to `info`.
#[must_use]
pub fn level_from_process(args: &[String], environ: &[String]) -> DetectedLevel {
    if let Some(value) = environ
        .iter()
        .find_map(|entry| entry.strip_prefix("RUST_LOG="))
    {
        // directives look like `komorebi=trace` or `info,komorebi=debug`; the most verbose wins
        let level = value
            .split(',')
            .filter_map(|directive| directive.rsplit('=').next())
            .filter_map(normalize_level)
            .max_by_key(|level| level_rank(level))
            .map_or_else(|| value.to_string(), str::to_string);

        return DetectedLevel {
            level,
            source: "RUST_LOG".to_string(),
        };
    }

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value = if let Some(value) = arg.strip_prefix("--log-level=") {
            Some(value)
        } else if arg == "--log-level" {
            iter.next().map(String::as_str)
        } else {
            None
        };

        if let Some(level) = value.and_then(normalize_level) {
            return DetectedLevel {
                level: level.to_string(),
                source: "--log-level".to_string(),
            };
        }
    }

    DetectedLevel {
        level: "info".to_string(),
        source: "default".to_string(),
    }
}

fn normalize_level(level: &str) -> Option<&'static str> {
    match level.to_ascii_lowercase().as_str() {
        "error" => Some("error"),
        "warn" => Some("warn"),
        "info" => Some("info"),
        "debug" => Some("debug"),
        "trace" => Some("trace"),
        _ => None,
    }
}

fn level_rank(level: &str) -> u8 {
    match level {
        "error" => 0,
        "warn" => 1,
        "info" => 2,
        "debug" => 3,
        _ => 4,
    }
}

/// A user-facing warning if the level is too quiet to explain a bug.
#[must_use]
pub fn level_warning(level: &str) -> Option<String> {
    (level_rank(&level.to_ascii_lowercase()) < level_rank("debug")).then(|| {
        format!(
            "komorebi is logging at {}; restart it with `--log-level trace` for full detail \
             (the recorder never changes komorebi's log level itself)",
            level.to_ascii_uppercase()
        )
    })
}

/// The path of `komorebi.log` inside a session directory (the concatenated slice).
#[must_use]
pub fn slice_path(session_dir: &Path) -> PathBuf {
    session_dir.join("komorebi.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, s)
            .single()
            .expect("valid time")
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn parses_tracing_timestamps() {
        let line = "2026-10-26T00:07:14.370960Z  INFO process_event: komorebi::process_event: processing event";
        let timestamp = parse_timestamp(line).expect("timestamp");
        assert_eq!(
            timestamp,
            utc(2026, 10, 26, 0, 7, 14) + Duration::microseconds(370_960)
        );
        assert_eq!(timestamp.to_rfc3339(), "2026-10-26T00:07:14.370960+00:00");
        assert_eq!(parse_level(line), Some("INFO"));
    }

    #[test]
    fn rejects_non_timestamped_lines() {
        for line in [
            "",
            "   0: backtrace::capture",
            "thread 'main' panicked at src/lib.rs:1:1:",
            "2026-10-26 00:07:14 not rfc3339",
        ] {
            assert_eq!(parse_timestamp(line), None, "{line:?}");
        }
    }

    #[test]
    fn continuation_lines_inherit_the_previous_timestamp() {
        let text = "2026-10-26T00:00:01.000000Z ERROR komorebi: panicked\n   0: frame\n   1: frame\n2026-10-26T00:00:02.000000Z  INFO next\n";
        let lines = index_lines(text.as_bytes());
        assert_eq!(lines.len(), 4);
        assert!(!lines[0].is_continuation);
        assert!(lines[1].is_continuation && lines[2].is_continuation);
        assert_eq!(lines[1].timestamp, lines[0].timestamp);
        assert_eq!(lines[2].timestamp, lines[0].timestamp);
        assert_ne!(lines[3].timestamp, lines[0].timestamp);
        assert_eq!(lines[1].offset, lines[0].len);
        assert_eq!(
            lines.iter().map(|line| line.len).sum::<u64>(),
            text.len() as u64
        );
    }

    #[test]
    fn leading_untimestamped_lines_have_no_time() {
        let lines = index_lines(b"orphan\n2026-10-26T00:00:02.000000Z  INFO x\n");
        assert_eq!(lines[0].timestamp, None);
        assert!(lines[1].timestamp.is_some());
    }

    fn slice_text(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("komorebi.log")).expect("slice")
    }

    #[test]
    fn follows_a_file_as_it_grows() {
        let dir = temp_dir("single");
        let path = dir.join(log_file_name(utc(2026, 10, 26, 12, 0, 0).date_naive()));
        std::fs::write(&path, "before\n").expect("write");

        let start = snapshot_start(&dir, utc(2026, 10, 26, 12, 0, 0));
        assert_eq!(start.offset, 7);
        let mut tailer = LogTailer::create(&dir, start, &dir.join("komorebi.log")).expect("create");

        assert_eq!(
            tailer.sync(utc(2026, 10, 26, 12, 0, 1)).expect("sync"),
            0,
            "nothing new yet"
        );

        std::fs::write(&path, "before\nduring 1\n").expect("write");
        assert_eq!(tailer.sync(utc(2026, 10, 26, 12, 0, 2)).expect("sync"), 9);
        assert_eq!(
            slice_text(&dir),
            "during 1\n",
            "what was already there is not part of the slice"
        );

        std::fs::write(&path, "before\nduring 1\nduring 2\n").expect("write");
        tailer.sync(utc(2026, 10, 26, 12, 0, 3)).expect("sync");
        tailer
            .sync(utc(2026, 10, 26, 12, 0, 4))
            .expect("sync again is a no-op");
        assert_eq!(slice_text(&dir), "during 1\nduring 2\n");

        let segments = tailer.segments();
        assert_eq!(segments.len(), 1);
        assert_eq!((segments[0].start_offset, segments[0].end_offset), (7, 25));
        assert_eq!(segments[0].len(), 18);
    }

    #[test]
    fn follows_across_a_utc_midnight_rollover() {
        let dir = temp_dir("rollover");
        let day1 = utc(2026, 10, 25, 23, 59, 50);
        let day1_path = dir.join(log_file_name(day1.date_naive()));
        let day2_path = dir.join(log_file_name(utc(2026, 10, 26, 0, 0, 0).date_naive()));
        std::fs::write(&day1_path, "old line\n").expect("write");

        let start = snapshot_start(&dir, day1);
        let mut tailer = LogTailer::create(&dir, start, &dir.join("komorebi.log")).expect("create");

        std::fs::write(&day1_path, "old line\nstart here\n").expect("write");
        tailer.sync(utc(2026, 10, 25, 23, 59, 55)).expect("sync");

        // komorebi appended to day 1, then rolled to day 2 at midnight UTC
        std::fs::write(&day1_path, "old line\nstart here\nlast of day 1\n").expect("write");
        std::fs::write(&day2_path, "first of day 2\n").expect("write");
        tailer.sync(utc(2026, 10, 26, 0, 0, 1)).expect("sync");

        std::fs::write(&day2_path, "first of day 2\nsecond of day 2\n").expect("write");
        tailer.sync(utc(2026, 10, 26, 0, 0, 10)).expect("sync");

        assert_eq!(
            slice_text(&dir),
            "start here\nlast of day 1\nfirst of day 2\nsecond of day 2\n"
        );

        let segments = tailer.segments();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].file, "komorebi_plaintext.log.2026-10-25");
        assert_eq!(segments[1].file, "komorebi_plaintext.log.2026-10-26");
        assert_eq!(segments[1].start_offset, 0);
        assert_eq!(segments[1].out_offset, segments[0].len());
    }

    #[test]
    fn a_straggling_write_to_yesterdays_file_does_not_scramble_the_slice() {
        let dir = temp_dir("straggler");
        let day1 = utc(2026, 10, 25, 23, 59, 50);
        let day1_path = dir.join(log_file_name(day1.date_naive()));
        let day2_path = dir.join(log_file_name(utc(2026, 10, 26, 0, 0, 0).date_naive()));
        std::fs::write(&day1_path, "").expect("write");

        let start = snapshot_start(&dir, day1);
        let mut tailer = LogTailer::create(&dir, start, &dir.join("komorebi.log")).expect("create");

        std::fs::write(&day1_path, "a\n").expect("write");
        std::fs::write(&day2_path, "b\n").expect("write");
        tailer.sync(utc(2026, 10, 26, 0, 0, 1)).expect("sync");

        // a late flush lands in day 1 after day 2 was already being followed
        std::fs::write(&day1_path, "a\nlate\n").expect("write");
        std::fs::write(&day2_path, "b\nc\n").expect("write");
        tailer.sync(utc(2026, 10, 26, 0, 0, 2)).expect("sync");

        assert_eq!(
            slice_text(&dir),
            "a\nb\nc\n",
            "the late line is dropped, the order is kept"
        );
    }

    #[test]
    fn a_missing_log_yields_no_segments_until_it_appears() {
        let dir = temp_dir("missing");
        let start = snapshot_start(&dir, utc(2026, 10, 26, 12, 0, 0));
        assert_eq!(start.offset, 0);
        assert!(plan_segments(&dir, &start, utc(2026, 10, 26, 12, 1, 0)).is_empty());

        let mut tailer = LogTailer::create(&dir, start, &dir.join("komorebi.log")).expect("create");
        assert_eq!(tailer.sync(utc(2026, 10, 26, 12, 0, 30)).expect("sync"), 0);
        assert!(tailer.segments().is_empty());

        // komorebi starts logging mid-recording
        std::fs::write(
            dir.join(log_file_name(utc(2026, 10, 26, 12, 0, 0).date_naive())),
            "hello\n",
        )
        .expect("write");
        tailer.sync(utc(2026, 10, 26, 12, 1, 0)).expect("sync");
        assert_eq!(slice_text(&dir), "hello\n");
    }

    #[test]
    fn detects_levels_from_the_command_line() {
        let args = |list: &[&str]| {
            list.iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>()
        };
        let none = args(&[]);

        assert_eq!(
            level_from_process(&args(&["komorebi"]), &none).level,
            "info"
        );
        assert_eq!(
            level_from_process(&args(&["komorebi", "--log-level", "trace"]), &none),
            DetectedLevel {
                level: "trace".into(),
                source: "--log-level".into()
            }
        );
        assert_eq!(
            level_from_process(&args(&["komorebi", "--log-level=debug"]), &none).level,
            "debug"
        );
    }

    #[test]
    fn rust_log_wins_over_the_flag() {
        let args = vec![
            "komorebi".to_string(),
            "--log-level".to_string(),
            "info".to_string(),
        ];
        let environ = vec!["HOME=/x".to_string(), "RUST_LOG=komorebi=trace".to_string()];
        let detected = level_from_process(&args, &environ);
        assert_eq!(
            detected,
            DetectedLevel {
                level: "trace".into(),
                source: "RUST_LOG".into()
            }
        );

        let environ = vec!["RUST_LOG=warn,komorebi=debug".to_string()];
        assert_eq!(level_from_process(&args, &environ).level, "debug");
    }

    #[test]
    fn warns_only_when_the_level_is_too_quiet() {
        assert!(
            level_warning("info")
                .expect("warning")
                .contains("--log-level trace")
        );
        assert!(level_warning("warn").is_some());
        assert!(level_warning("debug").is_none());
        assert!(level_warning("TRACE").is_none());
    }

    #[test]
    fn observes_levels_in_a_slice() {
        let text = "2026-10-26T00:00:01.000000Z  INFO a\n   continuation\n2026-10-26T00:00:02.000000Z DEBUG b\n";
        let levels = observed_levels(text);
        assert!(levels.contains("INFO") && levels.contains("DEBUG"));
        assert_eq!(levels.len(), 2);
    }
}

//! The macOS unified log, captured as a small set of `log stream` child processes.
//!
//! Instead of one mixed firehose there is one stream per *seam* we care about, each teed
//! untouched into its own file under `oslog/`:
//!
//! | stream     | predicate                                     | shows                                |
//! |------------|-----------------------------------------------|--------------------------------------|
//! | `komorebi` | `processID == <komorebi pid>`                 | system frameworks as the daemon sees them |
//! | `system`   | AX / HIServices / workspace subsystems        | the system side in between           |
//! | `app:*`    | `processID == <pid>`, one per managed app     | the same frameworks from the app's side |
//!
//! Debug-level events only exist live (not in `log show`), which is why this streams rather than
//! querying afterwards. `log stream` does not need root; `<private>` values stay redacted.
//!
//! The default `system` predicate was chosen by measuring event volume (debug level, 15 s, a real
//! desktop running komorebi):
//!
//! | candidate                                   | events/s | MB/min |
//! |---------------------------------------------|---------:|-------:|
//! | `com.apple.accessibility*`                  |      0.4 |   0.02 |
//! | `com.apple.SkyLight`                        |      2.4 |   0.14 |
//! | axserver / HIServices / workspace           |        0 |      0 |
//! | `com.apple.launchservices` (system-wide)    |      497 |   29.0 |
//! | `com.apple.runningboard` (system-wide)      |      289 |   16.0 |
//! | `WindowServer` process (all subsystems)     |     ~170 |  ~10   |
//!
//! so the cheap, relevant ones are the default and the noisy daemons are opt-in with
//! `--system-process`. (The three subsystems named in the original Console filters are silent in
//! practice, but cost nothing and may carry traffic on other macOS versions.)

use crate::clock;
use crate::incidents::Draft;
use crate::incidents::kind;
use clap::ValueEnum;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

/// The exact subsystems that carry the accessibility / window-server conversation.
pub const SYSTEM_SUBSYSTEMS: &[&str] = &[
    "com.apple.accessibility.axserver",
    "com.apple.HIServices",
    "com.apple.workspace",
    "com.apple.SkyLight",
];

/// Subsystem prefixes that are also part of it (`com.apple.accessibility.universalaccess`, ...).
pub const SYSTEM_SUBSYSTEM_PREFIXES: &[&str] = &["com.apple.accessibility"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OsLogLevel {
    Default,
    Info,
    #[default]
    Debug,
}

impl OsLogLevel {
    fn as_arg(self) -> &'static str {
        match self {
            OsLogLevel::Default => "default",
            OsLogLevel::Info => "info",
            OsLogLevel::Debug => "debug",
        }
    }
}

/// Which `log stream` child this is, and which timeline source its records belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamSpec {
    pub name: String,
    /// The timeline source label: `oslog:komorebi`, `oslog:system`, `oslog:app:<exe>`, `oslog:all`.
    pub source: String,
    pub predicate: String,
    pub level: OsLogLevel,
    /// Path of the raw ndjson relative to the session directory.
    pub file: String,
    pub pid: Option<i32>,
}

fn escape_predicate(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[must_use]
pub fn pid_predicate(pid: i32) -> String {
    format!("processID == {pid}")
}

/// The predicate for the `system` stream: the AX/workspace subsystems plus any explicitly
/// requested daemons.
#[must_use]
pub fn system_predicate(extra_processes: &[String]) -> String {
    let subsystems = SYSTEM_SUBSYSTEMS
        .iter()
        .map(|subsystem| format!("\"{subsystem}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let mut predicate = format!("subsystem IN {{{subsystems}}}");
    for prefix in SYSTEM_SUBSYSTEM_PREFIXES {
        predicate.push_str(&format!(" OR subsystem BEGINSWITH \"{prefix}\""));
    }

    for process in extra_processes {
        predicate.push_str(&format!(" OR process == \"{}\"", escape_predicate(process)));
    }

    predicate
}

/// Make a process name safe to use as a file name.
#[must_use]
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();

    let trimmed = cleaned.trim_matches(|c| c == '.' || c == '_');
    if trimmed.is_empty() {
        "app".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The stream for komorebi's process. komorebi can be restarted during a recording; each new
/// process gets its own file (`oslog/komorebi-<pid>.ndjson`) under the same `oslog:komorebi`
/// source, and the first keeps the plain name.
#[must_use]
pub fn komorebi_spec(pid: i32, level: OsLogLevel, first: bool) -> StreamSpec {
    StreamSpec {
        name: if first {
            "komorebi".to_string()
        } else {
            format!("komorebi-{pid}")
        },
        source: "oslog:komorebi".to_string(),
        predicate: pid_predicate(pid),
        level,
        file: if first {
            "oslog/komorebi.ndjson".to_string()
        } else {
            format!("oslog/komorebi-{pid}.ndjson")
        },
        pid: Some(pid),
    }
}

#[must_use]
pub fn system_spec(predicate: String, level: OsLogLevel) -> StreamSpec {
    StreamSpec {
        name: "system".to_string(),
        source: "oslog:system".to_string(),
        predicate,
        level,
        file: "oslog/system.ndjson".to_string(),
        pid: None,
    }
}

#[must_use]
pub fn app_spec(name: &str, pid: i32, level: OsLogLevel) -> StreamSpec {
    let file_name = sanitize_file_name(name);
    StreamSpec {
        name: format!("app:{name}-{pid}"),
        source: format!("oslog:app:{name}"),
        predicate: pid_predicate(pid),
        level,
        file: format!("oslog/apps/{file_name}-{pid}.ndjson"),
        pid: Some(pid),
    }
}

#[must_use]
pub fn all_spec(level: OsLogLevel) -> StreamSpec {
    StreamSpec {
        name: "all".to_string(),
        source: "oslog:all".to_string(),
        // `log stream` requires a predicate to be unfiltered-by-process; this matches everything
        predicate: "processID > 0".to_string(),
        level,
        file: "oslog/all.ndjson".to_string(),
        pid: None,
    }
}

/// What we keep of each unified-log record for indexing and display.
#[derive(Debug, Clone, Deserialize)]
pub struct OsLogRecord {
    #[serde(rename = "machTimestamp", default)]
    pub mach_ticks: u64,
    #[serde(default)]
    pub timestamp: String,
    #[serde(rename = "processID", default)]
    pub pid: i64,
    #[serde(rename = "processImagePath", default)]
    pub process_image_path: String,
    #[serde(default)]
    pub subsystem: String,
    #[serde(default)]
    pub category: String,
    #[serde(rename = "messageType", default)]
    pub message_type: String,
    #[serde(rename = "eventMessage", default)]
    pub message: String,
}

impl OsLogRecord {
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        if !line.starts_with('{') {
            return None;
        }

        serde_json::from_str(line).ok()
    }

    /// The record's time on the master (mach nanosecond) clock.
    #[must_use]
    pub fn mach_ns(&self) -> u64 {
        clock::timebase().ticks_to_ns(self.mach_ticks)
    }

    #[must_use]
    pub fn process_name(&self) -> &str {
        self.process_image_path
            .rsplit('/')
            .next()
            .unwrap_or(&self.process_image_path)
    }
}

/// Cheaply pull `"machTimestamp":<digits>` out of a raw line without a full JSON parse.
#[must_use]
pub fn extract_mach_ticks(line: &[u8]) -> Option<u64> {
    const KEY: &[u8] = b"\"machTimestamp\":";
    let start = line.windows(KEY.len()).position(|window| window == KEY)? + KEY.len();
    let digits: Vec<u8> = line[start..]
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();

    std::str::from_utf8(&digits).ok()?.parse().ok()
}

#[derive(Debug, Default)]
struct Counters {
    lines: AtomicU64,
    bytes: AtomicU64,
    /// `0` until the stream's header line has been seen.
    ready_mach_ns: AtomicU64,
    first_event_mach_ns: AtomicU64,
    last_event_mach_ns: AtomicU64,
}

/// The outcome of one stream, recorded in the session manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    #[serde(flatten)]
    pub spec: StreamSpec,
    /// When the `log stream` child was spawned (master clock).
    pub spawned_mach_ns: u64,
    /// When it announced it was filtering; events before this are not in the file.
    pub ready_mach_ns: Option<u64>,
    pub header: Option<String>,
    pub events: u64,
    pub bytes: u64,
    pub first_event_mach_ns: Option<u64>,
    pub last_event_mach_ns: Option<u64>,
    /// How many times the `log stream` child died and was brought back. Each restart leaves a
    /// gap in the file whose length is in the incident log.
    #[serde(default)]
    pub restarts: u32,
    pub error: Option<String>,
}

/// A running stream's live counters, for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamLive {
    pub name: String,
    pub events: u64,
    pub bytes: u64,
}

struct Running {
    spec: StreamSpec,
    child: Child,
    reader: Option<JoinHandle<Option<String>>>,
    stderr: Option<JoinHandle<String>>,
    counters: Arc<Counters>,
    spawned_mach_ns: u64,
    started: Instant,
    restarts: u32,
    /// Set once the stream has died for good: the reason, and no further restarts.
    lost: Option<String>,
}

/// How many times a stream that keeps dying is brought back before giving up on it.
const MAX_RESTARTS: u32 = 20;

/// A stream that dies sooner than this after starting has not been working at all (a bad
/// predicate, no access to the log), so restarting it would only loop.
const MIN_HEALTHY_RUNTIME: Duration = Duration::from_secs(2);

/// What a started `log stream` child consists of.
struct Launched {
    child: Child,
    reader: JoinHandle<Option<String>>,
    stderr: JoinHandle<String>,
    spawned_mach_ns: u64,
}

/// A set of concurrently running `log stream` children.
pub struct StreamSet {
    session_dir: std::path::PathBuf,
    running: Mutex<Vec<Running>>,
}

impl StreamSet {
    #[must_use]
    pub fn new(session_dir: &Path) -> Self {
        Self {
            session_dir: session_dir.to_path_buf(),
            running: Mutex::new(Vec::new()),
        }
    }

    /// Whether a stream with this name was already spawned.
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.running
            .lock()
            .iter()
            .any(|running| running.spec.name == name)
    }

    /// Whether a stream for this process id was already spawned.
    #[must_use]
    pub fn has_pid(&self, pid: i32) -> bool {
        self.running
            .lock()
            .iter()
            .any(|running| running.spec.pid == Some(pid))
    }

    /// Start `log stream` for `spec`, writing its records to the spec's file: truncating it for a
    /// new stream, appending for a restart so that nothing already captured is touched.
    fn launch(
        &self,
        spec: &StreamSpec,
        counters: &Arc<Counters>,
        append: bool,
    ) -> eyre::Result<Launched> {
        let path = self.session_dir.join(&spec.file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("could not create {}", parent.display()))?;
        }

        let file = if append {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        } else {
            File::create(&path)
        }
        .wrap_err_with(|| format!("could not open {}", path.display()))?;

        let spawned_mach_ns = clock::mach_now_ns();
        let mut child = Command::new("/usr/bin/log")
            .args([
                "stream",
                "--style",
                "ndjson",
                "--level",
                spec.level.as_arg(),
            ])
            .args(["--predicate", &spec.predicate])
            .stdin(Stdio::null())
            // own process group, so a terminal Ctrl-C reaches only us and we can drain and stop
            // these children in order instead of losing their tail
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .wrap_err("could not run /usr/bin/log")?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| eyre::Report::msg("log stream has no stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| eyre::Report::msg("log stream has no stderr"))?;

        let reader_counters = counters.clone();
        let reader = std::thread::Builder::new()
            .name(format!("oslog-{}", spec.name))
            .spawn(move || tee_stream(stdout, file, &reader_counters))
            .wrap_err("could not start the log reader thread")?;

        let stderr_reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });

        Ok(Launched {
            child,
            reader,
            stderr: stderr_reader,
            spawned_mach_ns,
        })
    }

    pub fn spawn(&self, spec: StreamSpec) -> eyre::Result<()> {
        let counters = Arc::new(Counters::default());
        let launched = self.launch(&spec, &counters, false)?;

        self.running.lock().push(Running {
            spec,
            child: launched.child,
            reader: Some(launched.reader),
            stderr: Some(launched.stderr),
            counters,
            spawned_mach_ns: launched.spawned_mach_ns,
            started: Instant::now(),
            restarts: 0,
            lost: None,
        });

        Ok(())
    }

    /// Notice `log stream` children that have exited on their own (logd restarting, the machine
    /// waking from sleep) and bring them back, appending to the same file so nothing already
    /// captured is lost. Events during the gap cannot be recovered, which is reported.
    pub fn reap(&self) -> Vec<Draft> {
        let mut drafts = Vec::new();
        let mut running = self.running.lock();

        for stream in running.iter_mut().filter(|stream| stream.lost.is_none()) {
            let Ok(Some(status)) = stream.child.try_wait() else {
                continue;
            };

            let exited_at = clock::mach_now_ns();
            let _ = stream.reader.take().map(JoinHandle::join);
            let stderr = stream
                .stderr
                .take()
                .and_then(|reader| reader.join().ok())
                .unwrap_or_default();
            let stderr = stderr.trim();

            let healthy = stream.started.elapsed() >= MIN_HEALTHY_RUNTIME;
            if !healthy || stream.restarts >= MAX_RESTARTS {
                let reason = if healthy {
                    format!(
                        "log stream kept dying (restarted {} times)",
                        stream.restarts
                    )
                } else {
                    format!("log stream exited immediately with {status}")
                };
                let reason = if stderr.is_empty() {
                    reason
                } else {
                    format!("{reason}: {stderr}")
                };

                drafts.push(Draft::new(
                    kind::LOG_STREAM_LOST,
                    format!("OS trace stream '{}': {reason}", stream.spec.name),
                ));
                stream.lost = Some(reason);
                continue;
            }

            let counters = stream.counters.clone();
            match self.launch(&stream.spec, &counters, true) {
                Ok(launched) => {
                    stream.child = launched.child;
                    stream.reader = Some(launched.reader);
                    stream.stderr = Some(launched.stderr);
                    stream.started = Instant::now();
                    stream.restarts += 1;

                    let last_event = counters.last_event_mach_ns.load(Ordering::Relaxed);
                    let gap = if last_event == 0 {
                        String::new()
                    } else {
                        format!(
                            "; events between {:.1}s before this and the restart are missing",
                            exited_at.saturating_sub(last_event) as f64 / 1e9
                        )
                    };
                    drafts.push(Draft::new(
                        kind::LOG_STREAM_RESTARTED,
                        format!(
                            "OS trace stream '{}' exited ({status}) and was restarted (restart {}){gap}",
                            stream.spec.name, stream.restarts
                        ),
                    ));
                }
                Err(error) => {
                    let reason = format!("could not restart log stream: {error:#}");
                    drafts.push(Draft::new(
                        kind::LOG_STREAM_LOST,
                        format!("OS trace stream '{}': {reason}", stream.spec.name),
                    ));
                    stream.lost = Some(reason);
                }
            }
        }

        drafts
    }

    /// Block until every stream has announced that it is filtering, or `timeout` passes. Returns
    /// the names of streams that were not ready in time.
    pub fn wait_ready(&self, timeout: Duration) -> Vec<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let pending: Vec<String> = self
                .running
                .lock()
                .iter()
                .filter(|running| running.counters.ready_mach_ns.load(Ordering::Relaxed) == 0)
                .map(|running| running.spec.name.clone())
                .collect();

            if pending.is_empty() || Instant::now() >= deadline {
                return pending;
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<StreamLive> {
        self.running
            .lock()
            .iter()
            .map(|running| StreamLive {
                name: running.spec.name.clone(),
                events: running.counters.lines.load(Ordering::Relaxed),
                bytes: running.counters.bytes.load(Ordering::Relaxed),
            })
            .collect()
    }

    fn info(stream: &Running, header: Option<String>, error: Option<String>) -> StreamInfo {
        let counters = &stream.counters;
        let nonzero = |value: u64| (value != 0).then_some(value);

        StreamInfo {
            spec: stream.spec.clone(),
            spawned_mach_ns: stream.spawned_mach_ns,
            ready_mach_ns: nonzero(counters.ready_mach_ns.load(Ordering::Relaxed)),
            header,
            events: counters.lines.load(Ordering::Relaxed),
            bytes: counters.bytes.load(Ordering::Relaxed),
            first_event_mach_ns: nonzero(counters.first_event_mach_ns.load(Ordering::Relaxed)),
            last_event_mach_ns: nonzero(counters.last_event_mach_ns.load(Ordering::Relaxed)),
            restarts: stream.restarts,
            error: error.or_else(|| stream.lost.clone()),
        }
    }

    /// Every stream's state so far, without stopping anything (for manifest checkpoints).
    #[must_use]
    pub fn checkpoint(&self) -> Vec<StreamInfo> {
        self.running
            .lock()
            .iter()
            .map(|stream| Self::info(stream, None, None))
            .collect()
    }

    /// Terminate every child, drain what they had buffered, and report each stream's outcome.
    pub fn stop(&self) -> Vec<StreamInfo> {
        let mut running = std::mem::take(&mut *self.running.lock());

        for stream in &running {
            // SAFETY: signalling a child process we spawned and still own
            unsafe {
                libc::kill(stream.child.id() as i32, libc::SIGTERM);
            }
        }

        running
            .iter_mut()
            .map(|stream| {
                let deadline = Instant::now() + Duration::from_secs(3);
                let status = loop {
                    match stream.child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        _ => {
                            let _ = stream.child.kill();
                            break stream.child.wait().ok();
                        }
                    }
                };

                let header = stream
                    .reader
                    .take()
                    .and_then(|reader| reader.join().ok().flatten());
                let stderr = stream
                    .stderr
                    .take()
                    .and_then(|reader| reader.join().ok())
                    .unwrap_or_default();

                // SIGTERM is how we stop it, so only other failures are interesting
                let mut error = None;
                if stream.lost.is_none()
                    && let Some(status) = status
                    && !status.success()
                {
                    use std::os::unix::process::ExitStatusExt;
                    if status.signal() != Some(libc::SIGTERM) {
                        error = Some(format!("log stream exited with {status}"));
                    }
                }

                let stderr = stderr.trim();
                if stream.lost.is_none() && !stderr.is_empty() {
                    error = Some(match error {
                        Some(existing) => format!("{existing}: {stderr}"),
                        None => stderr.to_string(),
                    });
                }

                Self::info(stream, header, error)
            })
            .collect()
    }
}

impl Drop for StreamSet {
    fn drop(&mut self) {
        // never leave `log stream` children behind if recording is abandoned
        if !self.running.lock().is_empty() {
            let _ = self.stop();
        }
    }
}

/// Copy `log stream`'s stdout into `file`, keeping only ndjson records in the file, and return the
/// non-JSON "Filtering the log data using ..." header if one was printed.
fn tee_stream(stdout: impl Read, file: File, counters: &Counters) -> Option<String> {
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    let mut line = Vec::with_capacity(4096);
    let mut header = None;

    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }

        if line.first() == Some(&b'{') {
            if writer.write_all(&line).is_err() {
                break;
            }

            counters.lines.fetch_add(1, Ordering::Relaxed);
            counters
                .bytes
                .fetch_add(line.len() as u64, Ordering::Relaxed);

            if let Some(ticks) = extract_mach_ticks(&line) {
                let ns = clock::timebase().ticks_to_ns(ticks);
                let _ = counters.first_event_mach_ns.compare_exchange(
                    0,
                    ns,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
                counters.last_event_mach_ns.store(ns, Ordering::Relaxed);
            }
        } else if header.is_none() {
            header = Some(String::from_utf8_lossy(&line).trim().to_string());
            // only the first: later headers are restarts, and the manifest wants when events
            // started being captured
            let _ = counters.ready_mach_ns.compare_exchange(
                0,
                clock::mach_now_ns(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }

        // flush whenever we have caught up with the pipe so a crash loses almost nothing
        if reader.buffer().is_empty() {
            let _ = writer.flush();
        }
    }

    let _ = writer.flush();
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `log stream --style ndjson` emits, captured from a real run (trimmed).
    const FIXTURE: &str = r#"{"timezoneName":"","messageType":"Debug","eventType":"logEvent","source":null,"formatString":"found no value for key %{public}@","userID":501,"activityIdentifier":0,"subsystem":"com.apple.defaults","category":"User Defaults","threadID":132217281,"senderImageUUID":"ADB84143-D965-3B3D-9BA5-EAB165A96490","backtrace":{"frames":[{"imageOffset":2047056,"imageUUID":"ADB84143"}]},"bootUUID":"","processImagePath":"\/Users\/lgug2z\/.cargo\/bin\/komorebi","timestamp":"2026-10-26 12:48:36.736892-0700","senderImagePath":"\/System\/Library\/Frameworks\/CoreFoundation","machTimestamp":29042701633017,"eventMessage":"found no value for key NSPersistentUIShowQuietSafeQuitStatus","processImageUUID":"X","traceID":40312880901652996,"processID":41666,"senderProgramCounter":2047056,"parentActivityIdentifier":0}"#;

    #[test]
    fn parses_a_real_record() {
        let record = OsLogRecord::parse(FIXTURE).expect("record");
        assert_eq!(record.mach_ticks, 29_042_701_633_017);
        assert_eq!(record.pid, 41666);
        assert_eq!(record.subsystem, "com.apple.defaults");
        assert_eq!(record.category, "User Defaults");
        assert_eq!(record.process_name(), "komorebi");
        assert!(record.message.starts_with("found no value"));
        assert_eq!(record.timestamp, "2026-10-26 12:48:36.736892-0700");
    }

    #[test]
    fn non_json_lines_are_not_records() {
        assert!(OsLogRecord::parse("Filtering the log data using \"processID == 1\"").is_none());
        assert!(OsLogRecord::parse("").is_none());
        assert!(OsLogRecord::parse("{not json").is_none());
    }

    #[test]
    fn extracts_mach_ticks_without_a_full_parse() {
        assert_eq!(
            extract_mach_ticks(FIXTURE.as_bytes()),
            Some(29_042_701_633_017)
        );
        assert_eq!(extract_mach_ticks(b"{\"nothing\":1}"), None);
    }

    #[test]
    fn builds_predicates() {
        assert_eq!(pid_predicate(42), "processID == 42");
        let system = system_predicate(&[]);
        for subsystem in SYSTEM_SUBSYSTEMS {
            assert!(system.contains(subsystem));
        }
        assert!(system.contains("subsystem BEGINSWITH \"com.apple.accessibility\""));
        assert!(!system.contains("process =="));

        let with_daemons =
            system_predicate(&["WindowServer".to_string(), "weird\"name".to_string()]);
        assert!(with_daemons.contains(" OR process == \"WindowServer\""));
        assert!(with_daemons.contains("weird\\\"name"));
    }

    #[test]
    fn sanitizes_app_names_for_files() {
        assert_eq!(sanitize_file_name("Brave Beta"), "Brave_Beta");
        assert_eq!(sanitize_file_name("IntelliJ IDEA"), "IntelliJ_IDEA");
        assert_eq!(sanitize_file_name("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_file_name("///"), "app");
        assert_eq!(sanitize_file_name(""), "app");
    }

    #[test]
    fn app_specs_are_labelled_by_exe_and_pid() {
        let spec = app_spec("Brave Beta", 1234, OsLogLevel::Debug);
        assert_eq!(spec.name, "app:Brave Beta-1234");
        assert_eq!(spec.source, "oslog:app:Brave Beta");
        assert_eq!(spec.file, "oslog/apps/Brave_Beta-1234.ndjson");
        assert_eq!(spec.predicate, "processID == 1234");
    }

    #[test]
    fn tee_keeps_records_and_splits_off_the_header() {
        let dir =
            std::env::temp_dir().join(format!("komorebi-recorder-tee-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("out.ndjson");

        let input = format!("Filtering the log data using \"x\"\n{FIXTURE}\n{FIXTURE}\n");
        let counters = Counters::default();
        let header = tee_stream(
            input.as_bytes(),
            File::create(&path).expect("file"),
            &counters,
        );

        assert_eq!(
            header.as_deref(),
            Some("Filtering the log data using \"x\"")
        );
        assert_eq!(counters.lines.load(Ordering::Relaxed), 2);
        assert_ne!(counters.ready_mach_ns.load(Ordering::Relaxed), 0);
        assert_ne!(counters.first_event_mach_ns.load(Ordering::Relaxed), 0);

        let written = std::fs::read_to_string(&path).expect("read");
        assert_eq!(written.lines().count(), 2);
        assert!(written.lines().all(|line| line.starts_with('{')));
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-oslog-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("oslog/apps")).expect("dir");
        dir
    }

    /// A real `log stream` on launchd that has become ready, or `None` where the unified log is
    /// not readable (a sandboxed build), in which case there is nothing to test.
    fn ready_stream(dir: &Path) -> Option<StreamSet> {
        let set = StreamSet::new(dir);
        let usable = set.spawn(app_spec("launchd", 1, OsLogLevel::Debug)).is_ok()
            && set.wait_ready(Duration::from_secs(5)).is_empty();
        if !usable {
            eprintln!("skipping: `log stream` is not usable from this environment");
            let _ = set.stop();
            return None;
        }

        Some(set)
    }

    fn child_pid(set: &StreamSet) -> i32 {
        set.running.lock()[0].child.id() as i32
    }

    #[test]
    fn a_log_stream_that_dies_is_restarted_into_the_same_file() {
        let dir = temp("restart");
        let Some(set) = ready_stream(&dir) else {
            return;
        };

        // only a stream that had been healthy for a while is worth bringing back
        std::thread::sleep(MIN_HEALTHY_RUNTIME + Duration::from_millis(300));
        let file = dir.join("oslog/apps/launchd-1.ndjson");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .and_then(|mut file| {
                std::io::Write::write_all(&mut file, b"{\"marker\":\"before the restart\"}\n")
            })
            .expect("marker");

        // logd restarts, or the machine wakes: the child is gone
        // SAFETY: signalling a child process this test spawned
        unsafe { libc::kill(child_pid(&set), libc::SIGKILL) };
        std::thread::sleep(Duration::from_millis(300));

        let drafts = set.reap();
        assert_eq!(drafts.len(), 1, "{drafts:?}");
        assert_eq!(drafts[0].kind, kind::LOG_STREAM_RESTARTED);
        assert!(
            drafts[0].detail.contains("restarted"),
            "{}",
            drafts[0].detail
        );

        // it is running again, and what was captured before is untouched
        assert!(set.wait_ready(Duration::from_secs(5)).is_empty());
        assert_eq!(set.checkpoint()[0].restarts, 1);
        assert!(set.reap().is_empty(), "the replacement is healthy");

        let infos = set.stop();
        assert_eq!(infos[0].restarts, 1);
        assert!(infos[0].error.is_none(), "{:?}", infos[0].error);
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(
            text.contains("before the restart"),
            "the restart must append, not truncate"
        );
    }

    #[test]
    fn a_log_stream_that_never_worked_is_reported_lost_not_restarted_forever() {
        let dir = temp("lost");
        let set = StreamSet::new(&dir);
        let spec = StreamSpec {
            name: "bad".to_string(),
            source: "oslog:bad".to_string(),
            predicate: "this is not ( a predicate".to_string(),
            level: OsLogLevel::Debug,
            file: "oslog/bad.ndjson".to_string(),
            pid: None,
        };

        if set.spawn(spec).is_err() {
            eprintln!("skipping: /usr/bin/log cannot be run from this environment");
            return;
        }

        // an invalid predicate makes `log stream` exit straight away
        std::thread::sleep(Duration::from_millis(800));

        let drafts = set.reap();
        assert_eq!(drafts.len(), 1, "{drafts:?}");
        assert_eq!(drafts[0].kind, kind::LOG_STREAM_LOST);
        assert!(
            drafts[0].detail.contains("exited immediately"),
            "{}",
            drafts[0].detail
        );

        // and it is not tried again, nor reported again
        assert!(set.reap().is_empty());
        let infos = set.stop();
        assert_eq!(infos[0].restarts, 0);
        assert!(
            infos[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("exited immediately"))
        );
    }

    #[test]
    fn the_first_ready_time_survives_a_restart() {
        let counters = Counters::default();
        let _ =
            counters
                .ready_mach_ns
                .compare_exchange(0, 111, Ordering::Relaxed, Ordering::Relaxed);

        // a restarted stream's header must not overwrite when events first started being captured
        let input = "Filtering the log data using \"x\"\n";
        let dir = temp("ready");
        tee_stream(
            input.as_bytes(),
            File::create(dir.join("a.ndjson")).expect("file"),
            &counters,
        );
        assert_eq!(counters.ready_mach_ns.load(Ordering::Relaxed), 111);
    }
}

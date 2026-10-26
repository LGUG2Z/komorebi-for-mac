//! The on-disk session directory and its manifest (`session.json`).
//!
//! Raw data is never duplicated or rewritten: `timeline.jsonl` only *references* byte ranges in
//! the raw files, so streams stay separate and can be reconciled or re-indexed later.

use crate::capture::DisplayResult;
use crate::clock::Anchor;
use crate::clock::Timebase;
use crate::sources::komorebi_events::EventSummary;
use crate::sources::komorebi_log::DetectedLevel;
use crate::sources::komorebi_log::LogSegment;
use crate::sources::komorebi_version::KomorebiVersion;
use crate::sources::managed_apps::AppRecord;
use crate::sources::oslog::StreamInfo;
use chrono::DateTime;
use chrono::Utc;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde::Deserialize;
use serde::Serialize;
#[cfg(test)]
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;

pub const SCHEMA_VERSION: u32 = 1;

/// The recorder is (or died while) recording; the manifest is a checkpoint.
pub const STATUS_RECORDING: &str = "recording";
/// Stopped normally and fully indexed.
pub const STATUS_COMPLETE: &str = "complete";
/// The recorder died; `reindex` rebuilt the session from what was on disk.
pub const STATUS_RECOVERED: &str = "recovered";

fn complete_status() -> String {
    STATUS_COMPLETE.to_string()
}
pub const MANIFEST_FILE: &str = "session.json";
pub const TIMELINE_FILE: &str = "timeline.jsonl";

/// A point in time on both clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moment {
    pub mach_ns: u64,
    pub wall_us: i64,
}

impl Moment {
    #[must_use]
    pub fn now() -> Self {
        let anchor = Anchor::sample();
        Self {
            mach_ns: anchor.mach_ns,
            wall_us: anchor.wall_us,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KomorebiInfo {
    pub running: bool,
    pub pid: Option<i32>,
    /// Every komorebi process seen during the recording, in order (it can be restarted).
    #[serde(default)]
    pub pids: Vec<i32>,
    pub log_level: Option<DetectedLevel>,
    /// Which komorebi each of `pids` was, as its binary reported: what a log line means depends on
    /// the source it was built from.
    #[serde(default)]
    pub versions: Vec<KomorebiVersion>,
    /// The name this recorder registered as with komorebi; komorebi's DEBUG log names it when it
    /// pushes each notification, which ties an event to its handler exactly.
    #[serde(default)]
    pub subscriber: Option<String>,
    /// Levels that actually appear in the recorded slice of komorebi's log.
    pub observed_levels: Vec<String>,
    pub data_dir: String,
}

/// A display's capture plus where its file sits relative to the session's video time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayEntry {
    #[serde(flatten)]
    pub result: DisplayResult,
    /// Seconds between the session's video t=0 and this file's first frame (>= 0). A timestamp
    /// `t` in session video time is at `t - offset_s` in this segment's file.
    pub offset_s: f64,
    /// Session video time at which this segment ended, once it has.
    #[serde(default)]
    pub end_s: Option<f64>,
}

/// The outcome of the self-test that checks the video against the clocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    pub method: String,
    pub samples: Vec<CalibrationSample>,
    pub max_abs_error_ms: f64,
    pub tolerance_ms: f64,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationSample {
    pub video_s: f64,
    pub predicted_mach_ns: u64,
    pub observed_mach_ns: u64,
    pub error_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    /// `recording`, `complete` or `recovered`.
    #[serde(default = "complete_status")]
    pub status: String,
    pub recorder_version: String,
    pub macos_version: Option<String>,
    pub komorebi: KomorebiInfo,
    pub timebase: Timebase,
    pub anchors: Vec<Anchor>,
    pub start: Moment,
    pub stop: Option<Moment>,
    /// Session video t=0 on the mach clock: the earliest first frame across displays.
    pub video_t0_mach_ns: Option<u64>,
    /// When video capture was stopped on the mach clock.
    pub video_end_mach_ns: Option<u64>,
    pub displays: Vec<DisplayEntry>,
    pub log_segments: Vec<LogSegment>,
    pub oslog: Vec<StreamInfo>,
    pub apps: Vec<AppRecord>,
    pub events: EventSummary,
    /// `host-clock` when frame timestamps were verified to be on the mach clock.
    pub sync_quality: String,
    pub calibration: Option<Calibration>,
    pub warnings: Vec<String>,
}

impl Manifest {
    #[must_use]
    pub fn new(start: Moment, anchors: Vec<Anchor>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            status: STATUS_RECORDING.to_string(),
            recorder_version: env!("CARGO_PKG_VERSION").to_string(),
            macos_version: macos_version(),
            komorebi: KomorebiInfo::default(),
            timebase: crate::clock::timebase(),
            anchors,
            start,
            stop: None,
            video_t0_mach_ns: None,
            video_end_mach_ns: None,
            displays: Vec::new(),
            log_segments: Vec::new(),
            oslog: Vec::new(),
            apps: Vec::new(),
            events: EventSummary::default(),
            sync_quality: "host-clock".to_string(),
            calibration: None,
            warnings: Vec::new(),
        }
    }

    /// Install the display segments: every segment's offset from the session's video t=0 (the
    /// earliest first frame of all of them) and the session-level video start and end.
    pub fn apply_segments(&mut self, results: Vec<DisplayResult>) {
        self.video_t0_mach_ns = results.iter().filter_map(|r| r.video_t0_mach_ns).min();
        let all_ended =
            !results.is_empty() && results.iter().all(|r| r.video_end_mach_ns.is_some());
        self.video_end_mach_ns = if all_ended {
            results.iter().filter_map(|r| r.video_end_mach_ns).max()
        } else {
            None
        };

        let t0 = self.video_t0_mach_ns;
        self.displays = results
            .into_iter()
            .map(|result| {
                let offset_s = match (result.video_t0_mach_ns, t0) {
                    (Some(this), Some(first)) => this.saturating_sub(first) as f64 / 1e9,
                    _ => 0.0,
                };
                let end_s = result.video_end_mach_ns.map(|end| self.video_seconds(end));
                DisplayEntry {
                    result,
                    offset_s,
                    end_s,
                }
            })
            .collect();
    }

    /// The segment of `display` that has video at session video time `video_s`, if any. When a
    /// display was unplugged or the machine slept there is a gap with no file to look in.
    #[must_use]
    pub fn segment_at(&self, display: u32, video_s: f64) -> Option<&DisplayEntry> {
        // segment boundaries are sampled, not exact, so allow a frame either side
        const SLACK_S: f64 = 0.05;
        let (_, session_end) = self.video_span();

        self.displays
            .iter()
            .filter(|entry| entry.result.display.id == display && entry.result.frames > 0)
            .filter(|entry| {
                video_s >= entry.offset_s - SLACK_S
                    && video_s <= entry.end_s.unwrap_or(session_end) + SLACK_S
            })
            .max_by(|a, b| a.offset_s.total_cmp(&b.offset_s))
    }

    /// The distinct display ids that were recorded at any point.
    #[must_use]
    pub fn display_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .displays
            .iter()
            .map(|entry| entry.result.display.id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// The master clock value that is `t = 0` in video time. Falls back to the recording start if
    /// no frame was ever captured, so timelines remain usable for the non-video sources.
    #[must_use]
    pub fn video_t0(&self) -> u64 {
        self.video_t0_mach_ns.unwrap_or(self.start.mach_ns)
    }

    #[must_use]
    pub fn video_seconds(&self, mach_ns: u64) -> f64 {
        (i128::from(mach_ns) - i128::from(self.video_t0())) as f64 / 1e9
    }

    #[must_use]
    pub fn mach_at_video_seconds(&self, video_s: f64) -> u64 {
        let ns = i128::from(self.video_t0()) + (video_s * 1e9).round() as i128;
        ns.max(0) as u64
    }

    /// The span of video time covered by the recording, `(0, duration)`. The end is the moment
    /// video capture stopped (frames are variable-rate, so the last frame can be earlier).
    #[must_use]
    pub fn video_span(&self) -> (f64, f64) {
        let end = self
            .video_end_mach_ns
            .or(self.stop.map(|stop| stop.mach_ns))
            .map_or(0.0, |end| self.video_seconds(end));

        (0.0, end.max(0.0))
    }
}

fn macos_version() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let version = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// A session directory on disk.
#[derive(Debug, Clone)]
pub struct Session {
    pub dir: PathBuf,
}

/// The default parent of session directories: komorebi's data dir, under `recordings/`.
#[must_use]
pub fn default_recordings_dir() -> PathBuf {
    komorebi_client::DATA_DIR.join("recordings")
}

impl Session {
    /// Create a fresh session directory: `base` if given, otherwise
    /// `<komorebi data dir>/recordings/<YYYYmmddTHHMMSSZ>/`.
    pub fn create(base: Option<PathBuf>, now: DateTime<Utc>) -> eyre::Result<Self> {
        let dir = base.unwrap_or_else(|| {
            default_recordings_dir().join(now.format("%Y%m%dT%H%M%SZ").to_string())
        });

        if dir.join(MANIFEST_FILE).exists() {
            eyre::bail!(
                "{} already contains a session; choose a different --out",
                dir.display()
            );
        }

        for sub in ["oslog/apps", "states"] {
            std::fs::create_dir_all(dir.join(sub))
                .wrap_err_with(|| format!("could not create {}", dir.join(sub).display()))?;
        }

        Ok(Self { dir })
    }

    pub fn open(dir: impl Into<PathBuf>) -> eyre::Result<Self> {
        let dir = dir.into();
        if !dir.join(MANIFEST_FILE).is_file() {
            eyre::bail!(
                "{} is not a recording session (no {MANIFEST_FILE})",
                dir.display()
            );
        }

        Ok(Self { dir })
    }

    #[must_use]
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.dir.join(relative)
    }

    pub fn manifest(&self) -> eyre::Result<Manifest> {
        let path = self.path(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("could not read {}", path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .wrap_err_with(|| format!("{} is not a valid session manifest", path.display()))?;

        if manifest.schema_version > SCHEMA_VERSION {
            eyre::bail!(
                "{} was written by a newer komorebi-recorder (schema {}, this build reads up to {})",
                path.display(),
                manifest.schema_version,
                SCHEMA_VERSION
            );
        }

        Ok(manifest)
    }

    pub fn write_manifest(&self, manifest: &Manifest) -> eyre::Result<()> {
        let path = self.path(MANIFEST_FILE);
        let temporary = self.path(format!("{MANIFEST_FILE}.tmp"));
        std::fs::write(&temporary, serde_json::to_string_pretty(manifest)?)?;
        // rename so a reader (or a crash) never sees a half-written manifest
        std::fs::rename(&temporary, &path)?;
        Ok(())
    }

    pub fn write_readme(&self, manifest: &Manifest) -> eyre::Result<()> {
        std::fs::write(self.path("README.md"), readme(&self.dir, manifest))?;
        Ok(())
    }
}

/// The generated `README.md`: what every file is and the exact commands to query this session.
#[must_use]
pub fn readme(dir: &Path, manifest: &Manifest) -> String {
    let session = shell_quote(&dir.display().to_string());
    let (_, duration) = manifest.video_span();
    let mut out = String::new();

    out.push_str("# komorebi-recorder session\n\n");
    out.push_str(&format!(
        "Recorded with komorebi-recorder {} on macOS {}.\nVideo duration: about {:.1}s. Video time `00:00.000` is the first captured frame.\n\n",
        manifest.recorder_version,
        manifest.macos_version.as_deref().unwrap_or("?"),
        duration
    ));

    out.push_str("## How to read it\n\n");
    out.push_str(
        "Every second of video maps to the exact records from every other source. Use the\n\
         `komorebi-recorder` CLI from this binary (this directory is self-contained):\n\n",
    );
    out.push_str(&format!(
        "```sh\n\
         # what happened around 12.5s into the video? (komorebi log + events + OS trace, +-1s)\n\
         komorebi-recorder at {session} 12.5 --window 1s\n\
         # narrow to one app's OS trace, or one source\n\
         komorebi-recorder at {session} 00:12.500 --app Ghostty\n\
         komorebi-recorder at {session} 12.5 --source events\n\
         # a span\n\
         komorebi-recorder range {session} 10 15\n\
         # a per-second digest of the whole recording\n\
         komorebi-recorder summary {session}\n\
         # reverse: a log line / event / UTC time -> video timecode\n\
         komorebi-recorder locate {session} --log-line 120\n\
         komorebi-recorder locate {session} --event-seq 7\n\
         komorebi-recorder locate {session} --utc 2026-10-26T19:43:35.062613Z\n\
         # extract PNG frames to look at (needs ffmpeg)\n\
         komorebi-recorder frames {session} 12.5 14.0\n\
         # rebuild timeline.jsonl from the raw files\n\
         komorebi-recorder reindex {session}\n\
         ```\n\n\
         Add `--json` to any query for machine-readable output.\n\n"
    ));

    out.push_str("## Files\n\n");
    out.push_str("| File | What it is |\n|---|---|\n");
    out.push_str("| `session.json` | Manifest: versions, displays, clock anchors, log segments, OS-trace streams and apps, warnings |\n");
    out.push_str("| `video-<displayID>[-<n>].mp4` | Screen recording of one display. Variable frame rate: frames only exist when the screen changed. A display that was unplugged, whose stream stopped, or that changed resolution has several numbered segments, each a complete file; `session.json` lists every segment with its time span |\n");
    out.push_str("| `frames-<displayID>[-<n>].csv` | Every frame's host-clock timestamp (`pts_mach_ns`) and when it was received |\n");
    out.push_str("| `recorder.jsonl` | What happened to the recording environment: displays connected/disconnected, sleep/wake, screen lock, restarts of log streams or komorebi. Shown as the `recorder` source |\n");
    out.push_str("| `komorebi.log` | The exact byte slice of komorebi's own log for this recording (UTC timestamps) |\n");
    out.push_str("| `events.jsonl` | Every komorebi subscriber notification: `{seq, recv_wall_us, recv_mach_ns, event, state_id}` |\n");
    out.push_str("| `states/<id>.json` | Full komorebi `State`, stored only when it changed (referenced by `state_id`) |\n");
    out.push_str(
        "| `state-start.json`, `state-end.json` | `State` when recording started / stopped |\n",
    );
    out.push_str("| `oslog/komorebi.ndjson` | macOS unified log for the komorebi process (system frameworks as the daemon sees them) |\n");
    out.push_str("| `oslog/system.ndjson` | macOS unified log: accessibility / HIServices / workspace subsystems |\n");
    out.push_str("| `oslog/apps/<exe>-<pid>.ndjson` | macOS unified log for one managed application process |\n");
    out.push_str("| `timeline.jsonl` | Merged index of every record, sorted by time, pointing into the raw files |\n\n");

    out.push_str("## Gaps are expected\n\n");
    out.push_str(
        "Docking, undocking, sleep, wake and screen lock change what can be recorded. The recorder keeps\n\
         going through all of them: a display that disappears has its video finalized on the spot and a new\n\
         segment starts when it returns, and a `log stream` or komorebi that goes away is picked up again.\n\
         Whatever happened is in `recorder.jsonl` (`komorebi-recorder at <session> <time> --source recorder`).\n\
         While the machine sleeps the host clock pauses, so video time skips the sleep rather than showing\n\
         a gap.\n\n",
    );

    if manifest.status != STATUS_COMPLETE {
        out.push_str("## This session was recovered\n\n");
        out.push_str(
            "The recorder did not finish normally. Everything that had been written was kept (video is\n\
             fragmented so it plays up to the last fragment) and `reindex` rebuilt this session from it.\n\
             The end of the recording is approximate and a second or two of video may be missing.\n\n",
        );
    }

    out.push_str("## Clocks\n\n");
    out.push_str(
        "The master clock is the host monotonic clock (`mach_absolute_time`, ns). Video frame\n\
         timestamps and the unified log's `machTimestamp` are on it directly. komorebi's log lines\n\
         (UTC wall clock) are mapped onto it with the `anchors` in `session.json`. Subscriber events\n\
         are stamped on receipt; they lag the true event by komorebi's dispatch latency, so prefer the\n\
         matching `processing event` line in `komorebi.log` as the authoritative event time.\n\n",
    );

    if !manifest.warnings.is_empty() {
        out.push_str("## Warnings recorded\n\n");
        for warning in &manifest.warnings {
            out.push_str(&format!("- {warning}\n"));
        }
        out.push('\n');
    }

    out.push_str("## Privacy\n\n");
    out.push_str(
        "The video contains whatever was on screen, and application logs can contain window titles\n\
         and URLs. Review this directory before sharing it. `<private>` values in the OS log are\n\
         redacted by macOS (no sudo or SIP changes are made by this tool).\n",
    );

    out
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | '+'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn manifest() -> Manifest {
        let start = Moment {
            mach_ns: 1_000_000_000_000,
            wall_us: 1_800_000_000_000_000,
        };
        Manifest::new(
            start,
            vec![Anchor {
                wall_us: start.wall_us,
                mach_ns: start.mach_ns,
            }],
        )
    }

    #[test]
    fn video_time_is_relative_to_the_first_frame() {
        let mut manifest = manifest();
        manifest.video_t0_mach_ns = Some(1_002_000_000_000);

        assert!((manifest.video_seconds(1_004_500_000_000) - 2.5).abs() < 1e-9);
        assert!((manifest.video_seconds(1_001_000_000_000) + 1.0).abs() < 1e-9);
        assert_eq!(manifest.mach_at_video_seconds(2.5), 1_004_500_000_000);
    }

    #[test]
    fn video_time_falls_back_to_the_recording_start() {
        let manifest = manifest();
        assert_eq!(manifest.video_t0(), 1_000_000_000_000);
    }

    #[test]
    fn manifests_roundtrip_through_json() {
        let manifest = manifest();
        let text = serde_json::to_string_pretty(&manifest).expect("serialize");
        let back: Manifest = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, manifest);
    }

    #[test]
    fn creates_opens_and_refuses_to_overwrite_a_session() {
        let base =
            std::env::temp_dir().join(format!("komorebi-recorder-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let now = Utc
            .with_ymd_and_hms(2026, 10, 26, 19, 43, 35)
            .single()
            .expect("time");

        let session = Session::create(Some(base.clone()), now).expect("create");
        assert!(session.path("oslog/apps").is_dir());
        assert!(session.path("states").is_dir());
        assert!(Session::open(&base).is_err(), "no manifest yet");

        session.write_manifest(&manifest()).expect("write");
        assert!(!session.path("session.json.tmp").exists());
        assert_eq!(
            Session::open(&base)
                .expect("open")
                .manifest()
                .expect("manifest"),
            manifest()
        );
        assert!(
            Session::create(Some(base), now).is_err(),
            "must not clobber a session"
        );
    }

    #[test]
    fn rejects_manifests_from_the_future() {
        let base =
            std::env::temp_dir().join(format!("komorebi-recorder-future-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let session = Session::create(Some(base), Utc::now()).expect("create");

        let mut future = manifest();
        future.schema_version = SCHEMA_VERSION + 1;
        session.write_manifest(&future).expect("write");
        let error = session.manifest().expect_err("too new").to_string();
        assert!(error.contains("newer komorebi-recorder"), "{error}");
    }

    #[test]
    fn the_readme_embeds_the_session_path_and_every_query() {
        let text = readme(Path::new("/tmp/a b/session"), &manifest());
        assert!(
            text.contains("'/tmp/a b/session'"),
            "paths with spaces are quoted"
        );
        for command in [
            "at ", "range ", "summary ", "locate ", "frames ", "reindex ",
        ] {
            assert!(
                text.contains(&format!("komorebi-recorder {command}")),
                "{command}"
            );
        }
    }

    fn segment(display: u32, number: u32, t0: u64, end: Option<u64>) -> DisplayResult {
        DisplayResult {
            display: crate::capture::DisplayDescriptor {
                id: display,
                frame: [0.0, 0.0, 100.0, 100.0],
                width_points: 100,
                height_points: 100,
                asleep: false,
            },
            segment: number,
            scale: 1.0,
            width_px: 100,
            height_px: 100,
            codec: crate::capture::Codec::Hevc,
            file: crate::capture::video_file_name(display, number),
            frames_file: crate::capture::frames_file_name(display, number),
            video_t0_mach_ns: Some(t0),
            video_end_mach_ns: end,
            end_reason: end.map(|_| crate::capture::EndReason::Stopped),
            frames: 10,
            dropped: 0,
            idle_callbacks: 0,
            latency: crate::capture::LatencyStats::default(),
            error: None,
        }
    }

    const S: u64 = 1_000_000_000;

    /// Display 1 is recorded for 0-10s, unplugged, and back for 30-40s; display 2 for 2-40s.
    fn docked_and_undocked() -> Manifest {
        let mut manifest = manifest();
        manifest.apply_segments(vec![
            segment(1, 1, 100 * S, Some(110 * S)),
            segment(2, 1, 102 * S, Some(140 * S)),
            segment(1, 2, 130 * S, Some(140 * S)),
        ]);
        manifest.video_end_mach_ns = Some(140 * S);
        manifest
    }

    #[test]
    fn segments_are_placed_relative_to_the_first_frame_of_any_of_them() {
        let manifest = docked_and_undocked();
        assert_eq!(manifest.video_t0_mach_ns, Some(100 * S));

        let by_file = |file: &str| {
            manifest
                .displays
                .iter()
                .find(|entry| entry.result.file == file)
                .expect("segment")
        };
        assert!((by_file("video-1.mp4").offset_s - 0.0).abs() < 1e-9);
        assert!((by_file("video-2.mp4").offset_s - 2.0).abs() < 1e-9);
        assert!((by_file("video-1-2.mp4").offset_s - 30.0).abs() < 1e-9);
        assert_eq!(by_file("video-1.mp4").end_s, Some(10.0));
        assert_eq!(by_file("video-1-2.mp4").end_s, Some(40.0));
        assert_eq!(manifest.display_ids(), vec![1, 2]);
        assert_eq!(manifest.video_span(), (0.0, 40.0));
    }

    #[test]
    fn the_right_segment_is_found_and_a_gap_has_none() {
        let manifest = docked_and_undocked();

        let at = |display: u32, t: f64| {
            manifest
                .segment_at(display, t)
                .map(|entry| entry.result.file.as_str())
        };

        assert_eq!(at(1, 5.0), Some("video-1.mp4"));
        assert_eq!(at(1, 35.0), Some("video-1-2.mp4"));
        assert_eq!(
            at(1, 20.0),
            None,
            "display 1 was unplugged between 10s and 30s"
        );
        assert_eq!(at(2, 20.0), Some("video-2.mp4"), "display 2 never stopped");
        assert_eq!(at(2, 1.0), None, "display 2 had not started yet");
        assert_eq!(at(9, 5.0), None, "no such display");

        // a frame of slack at each boundary absorbs sampling error
        assert_eq!(at(1, 10.03), Some("video-1.mp4"));
        assert_eq!(at(1, 10.2), None);
    }

    #[test]
    fn a_segment_still_being_recorded_extends_to_the_end_of_the_session() {
        let mut manifest = manifest();
        manifest.apply_segments(vec![segment(1, 1, 100 * S, None)]);
        manifest.stop = Some(Moment {
            mach_ns: 160 * S,
            wall_us: 0,
        });
        manifest.video_end_mach_ns = Some(160 * S);

        assert!(manifest.segment_at(1, 50.0).is_some());
        assert!(manifest.segment_at(1, 59.9).is_some());
    }

    #[test]
    fn manifests_from_before_segments_and_status_existed_still_load() {
        // exactly the shape a pre-segment recorder wrote: no `status`, no segment fields
        let mut value = serde_json::to_value(manifest()).expect("value");
        let object = value.as_object_mut().expect("object");
        object.remove("status");
        object.remove("video_end_mach_ns");
        object
            .get_mut("komorebi")
            .and_then(Value::as_object_mut)
            .expect("komorebi")
            .remove("pids");

        let old: Manifest = serde_json::from_value(value).expect("an old manifest parses");
        assert_eq!(
            old.status, STATUS_COMPLETE,
            "an old manifest was a finished recording"
        );
        assert!(old.komorebi.pids.is_empty());
    }
}

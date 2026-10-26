//! Building synthetic sessions for tests, without a screen, komorebi or a recorder.
//!
//! A real recording needs screen-capture permission and a running komorebi, so everything that
//! reads a session is tested against sessions assembled here: a manifest with a clock, komorebi
//! subscriber events with their states, a plaintext log, and recorder incidents, all placed at
//! chosen moments of video time. Not part of the public interface.

use crate::clock::Anchor;
use crate::incidents::INCIDENTS_FILE;
use crate::incidents::Incident;
use crate::session::Manifest;
use crate::session::Moment;
use crate::session::Session;
use crate::sources::komorebi_events::EventRow;
use crate::sources::komorebi_events::state_file_name;
use crate::timeline;
use chrono::Utc;
use serde_json::Value;
use std::path::PathBuf;

/// Video t=0 on the mach clock.
pub const T0_MACH_NS: u64 = 5_000_000_000;
/// What the wall clock reads at mach 0, in microseconds: 2026-10-26T12:00:00Z.
pub const WALL_AT_MACH_ZERO_US: i64 = 1_793_016_000_000_000;

/// The mach time of `video_s` seconds into the video.
#[must_use]
pub fn mach_ns(video_s: f64) -> u64 {
    (T0_MACH_NS as i64 + (video_s * 1e9).round() as i64).max(0) as u64
}

/// The wall time, in microseconds, of `video_s` seconds into the video.
#[must_use]
pub fn wall_us(video_s: f64) -> i64 {
    WALL_AT_MACH_ZERO_US + (mach_ns(video_s) / 1_000) as i64
}

struct PendingEvent {
    video_s: f64,
    event: Value,
    state: Option<Value>,
}

/// A session under construction. Times are seconds of video time.
pub struct SessionBuilder {
    dir: PathBuf,
    length_s: f64,
    events: Vec<PendingEvent>,
    log: Vec<(f64, String, String)>,
    incidents: Vec<Incident>,
}

impl SessionBuilder {
    /// A session in a fresh temporary directory, 10 s of video starting 1 s after the recorder
    /// started and ending 1 s before it stopped.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-testkit-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        Self {
            dir,
            length_s: 10.0,
            events: Vec::new(),
            log: Vec::new(),
            incidents: Vec::new(),
        }
    }

    #[must_use]
    pub fn length(mut self, seconds: f64) -> Self {
        self.length_s = seconds;
        self
    }

    /// A komorebi subscriber event received at `video_s`, with the state after it if it changed.
    #[must_use]
    pub fn event(mut self, video_s: f64, event: Value, state: Option<Value>) -> Self {
        self.events.push(PendingEvent {
            video_s,
            event,
            state,
        });
        self
    }

    /// One line of komorebi's plaintext log at `video_s`, e.g. `log(2.0, "INFO", "komorebi::x: hello")`.
    #[must_use]
    pub fn log(mut self, video_s: f64, level: &str, message: &str) -> Self {
        self.log
            .push((video_s, level.to_string(), message.to_string()));
        self
    }

    #[must_use]
    pub fn incident(mut self, video_s: f64, kind: &str, detail: &str) -> Self {
        self.incidents.push(Incident {
            mach_ns: mach_ns(video_s),
            wall_us: wall_us(video_s),
            kind: kind.to_string(),
            detail: detail.to_string(),
            display: None,
        });
        self
    }

    /// Write the session and index its timeline.
    #[must_use]
    pub fn build(mut self) -> (Session, Manifest) {
        let session = Session::create(Some(self.dir.clone()), Utc::now()).expect("create");

        let start = Moment {
            mach_ns: mach_ns(-1.0),
            wall_us: wall_us(-1.0),
        };
        let stop = Moment {
            mach_ns: mach_ns(self.length_s + 1.0),
            wall_us: wall_us(self.length_s + 1.0),
        };
        let mut manifest = Manifest::new(
            start,
            vec![
                Anchor {
                    mach_ns: start.mach_ns,
                    wall_us: start.wall_us,
                },
                Anchor {
                    mach_ns: stop.mach_ns,
                    wall_us: stop.wall_us,
                },
            ],
        );
        manifest.stop = Some(stop);
        manifest.video_t0_mach_ns = Some(T0_MACH_NS);
        manifest.video_end_mach_ns = Some(mach_ns(self.length_s));
        manifest.events.events = self.events.len() as u64;

        self.events.sort_by(|a, b| a.video_s.total_cmp(&b.video_s));
        let mut state_id = 0_u64;
        let mut previous: Option<Value> = None;
        let mut rows = String::new();
        for (index, pending) in self.events.iter().enumerate() {
            if let Some(state) = &pending.state
                && previous.as_ref() != Some(state)
            {
                state_id += 1;
                std::fs::write(
                    session.path(state_file_name(state_id)),
                    serde_json::to_string(state).expect("state"),
                )
                .expect("state file");
                previous = Some(state.clone());
            }

            let row = EventRow {
                seq: index as u64 + 1,
                recv_wall_us: wall_us(pending.video_s),
                recv_mach_ns: mach_ns(pending.video_s),
                event: Some(pending.event.clone()),
                state_id: (state_id > 0).then_some(state_id),
                parse_error: None,
                raw: None,
            };
            rows.push_str(&serde_json::to_string(&row).expect("row"));
            rows.push('\n');
        }
        std::fs::write(session.path("events.jsonl"), rows).expect("events");

        self.log.sort_by(|a, b| a.0.total_cmp(&b.0));
        // tracing_subscriber right-aligns the level in five columns
        let log: String = self
            .log
            .iter()
            .map(|(video_s, level, message)| {
                format!(
                    "{} {level:>5} {message}\n",
                    timeline::format_wall_us(wall_us(*video_s)).expect("time"),
                )
            })
            .collect();
        std::fs::write(session.path("komorebi.log"), log).expect("log");

        self.incidents.sort_by_key(|incident| incident.mach_ns);
        let incidents: String = self
            .incidents
            .iter()
            .map(|incident| serde_json::to_string(incident).expect("incident") + "\n")
            .collect();
        std::fs::write(session.path(INCIDENTS_FILE), incidents).expect("incidents");

        session.write_manifest(&manifest).expect("manifest");
        timeline::build(&session).expect("timeline");
        (session, manifest)
    }
}

/// komorebi-style log text from `(video_s, level, text after the level)` lines: the timestamp and
/// the right-aligned level are added the way `tracing_subscriber` does.
#[must_use]
pub fn log_text(lines: &[(f64, &str, &str)]) -> String {
    lines
        .iter()
        .map(|(video_s, level, text)| {
            format!(
                "{} {level:>5} {text}\n",
                timeline::format_wall_us(wall_us(*video_s)).expect("time")
            )
        })
        .collect()
}

/// A minimal komorebi `State` with one monitor, one workspace and the given windows (hwnd, title).
#[must_use]
pub fn state_with_windows(windows: &[(u64, &str)]) -> Value {
    let containers: Vec<Value> = windows
        .iter()
        .map(|(hwnd, title)| {
            serde_json::json!({
                "id": format!("c{hwnd}"),
                "windows": {"elements": [{"id": hwnd, "title": title, "exe": "app", "pid": 100 + hwnd}], "focused": 0},
            })
        })
        .collect();
    serde_json::json!({
        "monitors": {"elements": [{
            "id": 1,
            "workspaces": {"elements": [{
                "name": "one",
                "containers": {"elements": containers, "focused": 0},
            }], "focused": 0},
        }], "focused": 0},
    })
}

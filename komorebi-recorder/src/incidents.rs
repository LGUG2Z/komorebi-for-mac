//! Things that happened to the *recording environment* while recording: a display was unplugged,
//! the machine slept and woke, the screen locked, a log stream was restarted, komorebi restarted.
//!
//! They are appended to `recorder.jsonl` the moment they are noticed (and flushed, so a crash
//! loses nothing) and indexed into the timeline as the `recorder` source. That puts "display 1
//! disconnected" or "woke after 42 s asleep" right next to the log lines and OS trace around it,
//! which is usually the first thing needed to explain a gap or a strange layout.

use crate::clock;
use serde::Deserialize;
use serde::Serialize;
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;

pub const INCIDENTS_FILE: &str = "recorder.jsonl";
pub const SOURCE_RECORDER: &str = "recorder";

/// The kinds of incident, as written to `recorder.jsonl`.
pub mod kind {
    pub const CAPTURE_STARTED: &str = "capture-started";
    pub const CAPTURE_STOPPED: &str = "capture-stopped";
    pub const CAPTURE_FAILED: &str = "capture-failed";
    pub const CAPTURE_RECOVERED: &str = "capture-recovered";
    pub const DISPLAY_CONNECTED: &str = "display-connected";
    pub const DISPLAY_DISCONNECTED: &str = "display-disconnected";
    pub const DISPLAY_RECONFIGURED: &str = "display-reconfigured";
    pub const DISPLAY_ASLEEP: &str = "display-asleep";
    pub const DISPLAY_AWAKE: &str = "display-awake";
    pub const WAKE: &str = "wake";
    pub const CLOCK_STEP: &str = "clock-step";
    pub const SCREEN_LOCKED: &str = "screen-locked";
    pub const SCREEN_UNLOCKED: &str = "screen-unlocked";
    pub const SESSION_INACTIVE: &str = "session-inactive";
    pub const SESSION_ACTIVE: &str = "session-active";
    pub const KOMOREBI_EXITED: &str = "komorebi-exited";
    pub const KOMOREBI_STARTED: &str = "komorebi-started";
    pub const LOG_STREAM_RESTARTED: &str = "log-stream-restarted";
    pub const LOG_STREAM_LOST: &str = "log-stream-lost";
    pub const RECORDER: &str = "recorder";
}

/// One line of `recorder.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    pub mach_ns: u64,
    pub wall_us: i64,
    pub kind: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<u32>,
}

/// An incident that has not been timestamped yet. The time it *happened* can be earlier than the
/// time it was noticed (a stream that died, a wake detected a moment later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub kind: &'static str,
    pub detail: String,
    pub display: Option<u32>,
    pub mach_ns: Option<u64>,
}

impl Draft {
    #[must_use]
    pub fn new(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            display: None,
            mach_ns: None,
        }
    }

    #[must_use]
    pub fn display(mut self, display: u32) -> Self {
        self.display = Some(display);
        self
    }

    #[must_use]
    pub fn at(mut self, mach_ns: u64) -> Self {
        self.mach_ns = Some(mach_ns);
        self
    }
}

/// How many recent incidents to keep for the UI.
const RECENT: usize = 8;

pub struct IncidentLog {
    writer: BufWriter<File>,
    recent: VecDeque<Incident>,
    count: u64,
}

impl IncidentLog {
    pub fn create(session_dir: &Path) -> std::io::Result<Self> {
        Ok(Self {
            writer: BufWriter::new(File::create(session_dir.join(INCIDENTS_FILE))?),
            recent: VecDeque::new(),
            count: 0,
        })
    }

    /// Timestamp, write and flush an incident.
    pub fn record(&mut self, draft: Draft) -> Incident {
        let now_mach = clock::mach_now_ns();
        let mach_ns = draft.mach_ns.unwrap_or(now_mach).min(now_mach);
        // the wall time it happened at, derived from how long ago that was
        let ago_us = i64::try_from(now_mach.saturating_sub(mach_ns) / 1_000).unwrap_or(0);
        let incident = Incident {
            mach_ns,
            wall_us: clock::wall_now_us() - ago_us,
            kind: draft.kind.to_string(),
            detail: draft.detail,
            display: draft.display,
        };

        if let Ok(line) = serde_json::to_string(&incident) {
            // incidents are rare and precious: never buffer them
            let _ = writeln!(self.writer, "{line}");
            let _ = self.writer.flush();
        }

        self.count += 1;
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(incident.clone());
        incident
    }

    pub fn record_all(&mut self, drafts: impl IntoIterator<Item = Draft>) {
        for draft in drafts {
            self.record(draft);
        }
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub fn recent(&self) -> Vec<Incident> {
        self.recent.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komorebi-recorder-incidents-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn incidents_are_on_disk_the_moment_they_are_recorded() {
        let dir = temp("flush");
        let mut log = IncidentLog::create(&dir).expect("create");

        log.record(Draft::new(kind::DISPLAY_DISCONNECTED, "display 1 went away").display(1));

        // no flush or drop needed: a crash right now must not lose it
        let text = std::fs::read_to_string(dir.join(INCIDENTS_FILE)).expect("read");
        let incident: Incident =
            serde_json::from_str(text.lines().next().expect("line")).expect("parse");
        assert_eq!(incident.kind, "display-disconnected");
        assert_eq!(incident.display, Some(1));
        assert_eq!(log.count(), 1);
    }

    #[test]
    fn an_incident_can_be_dated_before_it_was_noticed() {
        let dir = temp("dated");
        let mut log = IncidentLog::create(&dir).expect("create");

        let before = clock::mach_now_ns();
        let earlier = before - 5_000_000_000;
        let incident = log.record(Draft::new(kind::CAPTURE_STOPPED, "stream died").at(earlier));

        assert_eq!(incident.mach_ns, earlier);
        // the wall time is five seconds before now
        let skew = (clock::wall_now_us() - incident.wall_us - 5_000_000).abs();
        assert!(skew < 100_000, "wall time was {skew} us off");
    }

    #[test]
    fn an_incident_cannot_be_dated_in_the_future() {
        let dir = temp("future");
        let mut log = IncidentLog::create(&dir).expect("create");
        let incident = log.record(Draft::new(kind::WAKE, "x").at(u64::MAX));
        assert!(incident.mach_ns <= clock::mach_now_ns());
    }

    #[test]
    fn only_the_most_recent_incidents_are_kept_for_the_ui() {
        let dir = temp("recent");
        let mut log = IncidentLog::create(&dir).expect("create");
        for index in 0..20 {
            log.record(Draft::new(kind::RECORDER, format!("incident {index}")));
        }

        let recent = log.recent();
        assert_eq!(recent.len(), RECENT);
        assert_eq!(recent.last().expect("last").detail, "incident 19");
        assert_eq!(log.count(), 20);
    }
}

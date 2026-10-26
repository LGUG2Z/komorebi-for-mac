//! Answers shaped for a reader: what the commands print as JSON and the viewer draws.

use super::blocks::Quality;
use super::classify::Class;
use super::events::EventInfo;
use super::facade::Analysis;
use super::state::Snapshot;
use crate::timecode;
use schemars::JsonSchema;
use serde::Serialize;

/// The handler that produced an event, as far as the log shows.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct HandlerView {
    /// How the event was tied to this handler.
    pub quality: Quality,
    pub header_line: u64,
    pub last_line: u64,
    /// When the handler's header was logged, in video time.
    pub started_video_s: f64,
    /// At least how long the handler took: header to the event being received.
    pub handler_ms: Option<f64>,
    pub lines: usize,
    pub errors: u32,
    pub warnings: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct EventView {
    pub seq: u64,
    /// When the recorder received the notification, which is when the handler finished.
    pub video_s: f64,
    pub mach_ns: u64,
    pub wall_utc: Option<String>,
    #[serde(flatten)]
    pub info: EventInfo,
    pub summary: String,
    pub class: Class,
    pub reason: String,
    /// `None` when the state before or after was not recorded.
    pub state_changed: Option<bool>,
    pub state_id: Option<u64>,
    pub handler: Option<HandlerView>,
}

impl Analysis {
    /// Event number `index` of the session as a reader sees it.
    #[must_use]
    pub fn event_view(&self, index: usize) -> Option<EventView> {
        let event = self.events().get(index)?;
        let pairing = self.pairing();
        let classification = pairing.classes.get(index)?;
        let pair = pairing.pairs.get(index)?;
        let log = self.log();

        let handler = pair
            .block
            .and_then(|id| log.blocks.get(id))
            .map(|block| HandlerView {
                quality: pair.quality,
                header_line: block.header_line,
                last_line: block.last_line,
                started_video_s: self.video_s(block.header_mach_ns),
                handler_ms: pair.handler_ms,
                lines: block.lines.len(),
                errors: block.errors,
                warnings: block.warnings,
            });

        Some(EventView {
            seq: event.seq,
            video_s: self.video_s(event.recv_mach_ns),
            mach_ns: event.recv_mach_ns,
            wall_utc: self.wall_utc(event.recv_mach_ns),
            summary: event.info.summary(),
            info: event.info.clone(),
            class: classification.class,
            reason: classification.reason.clone(),
            state_changed: self.state_changed(index),
            state_id: event.state_id,
            handler,
        })
    }
}

/// A moment on every clock a reader might think in.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Moment {
    pub video_s: f64,
    pub timecode: String,
    pub mach_ns: u64,
    pub wall_utc: Option<String>,
}

impl Analysis {
    #[must_use]
    pub fn moment(&self, mach_ns: u64) -> Moment {
        let video_s = self.video_s(mach_ns);
        Moment {
            video_s,
            timecode: timecode::format(video_s),
            mach_ns,
            wall_utc: self.wall_utc(mach_ns),
        }
    }
}

/// Where a state shown to a reader came from.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct StateOrigin {
    /// `event` (the notification of that event) or `start` (the state when recording began).
    pub source: String,
    pub seq: Option<u64>,
    pub state_id: Option<u64>,
    /// How long before the asked-for moment the state was taken. komorebi can change state without
    /// sending a notification, so a large number means the state may be out of date.
    pub age_ms: Option<f64>,
}

/// A one-line description of a snapshot for headings.
#[must_use]
pub fn headline(snapshot: &Snapshot) -> String {
    let windows = snapshot.windows().len();
    let monitors = snapshot.monitors.len();
    format!(
        "{monitors} monitor{}, {windows} window{}",
        if monitors == 1 { "" } else { "s" },
        if windows == 1 { "" } else { "s" }
    )
}

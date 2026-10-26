//! One window's life through a recording: when it appeared, where komorebi put it, when it was
//! parked off screen or moved, and which event did each.
//!
//! Most bugs are about one window ("my terminal ended up behind the dock"), and the state at any
//! moment is dozens of windows. This follows the windows that match a query through every recorded
//! state and reports each moment something about them changed, with the event it came with.

use super::Analysis;
use super::classify::Class;
use super::envelope;
use super::state::Location;
use super::state::Place;
use super::state::Rect;
use super::state::Snapshot;
use super::state::Window;
use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

pub const SCHEMA_WINDOW: &str = "window/1";

/// A window rect that moved by no more than this is not reported as having moved.
const RECT_TOLERANCE: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TrackedWindow {
    pub id: u64,
    pub exe: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct HistoryEntry {
    pub window_id: u64,
    /// The event whose notification carried this state.
    pub seq: u64,
    pub video_s: f64,
    pub mach_ns: u64,
    pub event: String,
    pub class: Class,
    /// Where the window was after this event; `None` if it was gone.
    pub location: Option<Location>,
    /// Where it really was on screen after this event.
    pub rect: Option<Rect>,
    /// Parked in komorebi's hiding position.
    pub hidden: bool,
    /// What changed about it, in words.
    pub changes: Vec<String>,
    /// This event was *about* the window (it names its id), not just a bystander.
    pub about_window: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct HistoryData {
    pub query: String,
    pub windows: Vec<TrackedWindow>,
    pub entries: Vec<HistoryEntry>,
    /// How many entries there are after the cursor, including any not returned.
    pub matching: usize,
}

/// What about a window is worth noticing when it changes.
#[derive(Clone, PartialEq, Eq)]
struct Seen {
    monitor_id: u64,
    workspace: String,
    kind: &'static str,
    visible: bool,
    rect: Option<Rect>,
    hidden: bool,
    location: Location,
}

fn kind_of(place: &Place) -> &'static str {
    match place {
        Place::Container { .. } => "container",
        Place::Monocle { .. } => "monocle",
        Place::Maximized => "maximized",
        Place::Floating { .. } => "floating",
    }
}

fn seen(snapshot: &Snapshot, location: Location, window: &Window) -> Seen {
    let hidden = window.rect.as_ref().is_some_and(|rect| {
        snapshot
            .monitors
            .iter()
            .find(|monitor| monitor.id == location.monitor_id)
            .is_some_and(|monitor| Snapshot::is_hidden_position(rect, monitor))
    });
    Seen {
        monitor_id: location.monitor_id,
        workspace: location.workspace.clone(),
        kind: kind_of(&location.place),
        visible: location.workspace_visible,
        rect: window.rect,
        hidden,
        location,
    }
}

fn matches(query: &str, window: &Window) -> bool {
    if let Ok(id) = query.parse::<u64>()
        && window.id == id
    {
        return true;
    }

    let needle = query.to_lowercase();
    window.exe.to_lowercase().contains(&needle) || window.title.to_lowercase().contains(&needle)
}

impl Analysis {
    /// The history of the windows matching `query` (an id, or text in the application or title),
    /// for events after `after_seq`, at most `limit` entries.
    pub fn window_history(
        &self,
        query: &str,
        after_seq: Option<u64>,
        limit: Option<usize>,
    ) -> eyre::Result<HistoryData> {
        let mut tracked: BTreeMap<u64, TrackedWindow> = BTreeMap::new();
        let mut last: BTreeMap<u64, Option<Seen>> = BTreeMap::new();
        let mut entries: Vec<HistoryEntry> = Vec::new();
        let mut matching = 0_usize;

        let mut previous_state_id: Option<u64> = None;
        let mut previous_snapshot: Option<std::sync::Arc<Snapshot>> = None;

        for (index, event) in self.events().iter().enumerate() {
            let Some(state_id) = event.state_id else {
                continue;
            };

            // an unchanged state cannot have changed a window, but the event may be about it
            let snapshot = if previous_state_id == Some(state_id) {
                previous_snapshot.clone().ok_or_else(|| {
                    eyre::Report::msg("internal error: repeated state without a snapshot")
                })?
            } else {
                self.snapshot(state_id)?
            };
            previous_state_id = Some(state_id);
            previous_snapshot = Some(snapshot.clone());

            let class = self.pairing().classes[index].class;
            let here: BTreeMap<u64, (Location, &Window)> = snapshot
                .windows()
                .into_iter()
                .filter(|(_, window)| matches(query, window))
                .fold(BTreeMap::new(), |mut found, (location, window)| {
                    found.entry(window.id).or_insert((location, window));
                    found
                });

            for (id, (_, window)) in &here {
                tracked.entry(*id).or_insert_with(|| TrackedWindow {
                    id: *id,
                    exe: window.exe.clone(),
                    title: window.title.clone(),
                });
                last.entry(*id).or_insert(None);
            }

            let about = |id: u64| {
                event
                    .info
                    .window_id
                    .is_some_and(|window| u64::from(window) == id)
            };
            let ids: Vec<u64> = last.keys().copied().collect();
            for id in ids {
                let now = here
                    .get(&id)
                    .map(|(location, window)| seen(&snapshot, location.clone(), window));
                let before = last.get(&id).cloned().flatten();

                let mut changes = Vec::new();
                match (&before, &now) {
                    (None, Some(now)) => {
                        changes.push(format!("appeared on {}", now.location.describe()))
                    }
                    (Some(_), None) => changes.push("went away".to_string()),
                    (Some(was), Some(now)) => {
                        if (was.monitor_id, &was.workspace, was.kind)
                            != (now.monitor_id, &now.workspace, now.kind)
                        {
                            changes.push(format!(
                                "moved from {} to {}",
                                was.location.describe(),
                                now.location.describe()
                            ));
                        }
                        if was.visible != now.visible {
                            changes.push(
                                if now.visible {
                                    "its workspace came into view"
                                } else {
                                    "its workspace went out of view"
                                }
                                .to_string(),
                            );
                        }
                        if was.hidden != now.hidden {
                            changes.push(
                                if now.hidden {
                                    "parked off screen in komorebi's hiding position"
                                } else {
                                    "brought back on screen"
                                }
                                .to_string(),
                            );
                        }
                        if let (Some(from), Some(to)) = (&was.rect, &now.rect)
                            && from.max_delta(to) > RECT_TOLERANCE
                            && was.hidden == now.hidden
                        {
                            changes.push(format!(
                                "really at {} instead of {}",
                                to.describe(),
                                from.describe()
                            ));
                        }
                    }
                    (None, None) => {}
                }

                let relevant = !changes.is_empty() || (about(id) && now.is_some());
                if relevant && after_seq.is_none_or(|after| event.seq > after) {
                    matching += 1;
                    if limit.is_none_or(|limit| entries.len() < limit) {
                        entries.push(HistoryEntry {
                            window_id: id,
                            seq: event.seq,
                            video_s: self.video_s(event.recv_mach_ns),
                            mach_ns: event.recv_mach_ns,
                            event: event.info.summary(),
                            class,
                            location: now.as_ref().map(|seen| seen.location.clone()),
                            rect: now.as_ref().and_then(|seen| seen.rect),
                            hidden: now.as_ref().is_some_and(|seen| seen.hidden),
                            changes,
                            about_window: about(id),
                        });
                    }
                }
                last.insert(id, now);
            }
        }

        if tracked.is_empty() {
            return Err(envelope::not_found(format!(
                "no window matching '{query}' appears in any recorded state"
            )));
        }

        Ok(HistoryData {
            query: query.to_string(),
            windows: tracked.into_values().collect(),
            entries,
            matching,
        })
    }
}

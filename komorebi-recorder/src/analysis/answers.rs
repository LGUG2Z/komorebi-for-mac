//! The answers to "what was the state", "what changed" and "what happened", as data.
//!
//! Built here, from an [`Analysis`], so that the commands print them and the viewer draws the
//! same thing.

use super::Analysis;
use super::classify::Class;
use super::diff;
use super::diff::Change;
use super::envelope;
use super::facade::StateAt;
use super::facade::StateSource;
use super::invariants;
use super::invariants::Violation;
use super::state::Focus;
use super::state::Location;
use super::state::Monitor;
use super::state::Place;
use super::state::Rect;
use super::state::Snapshot;
use super::state::Window;
use super::views::EventView;
use super::views::Moment;
use super::views::StateOrigin;
use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;

pub const SCHEMA_STATE: &str = "state/1";
pub const SCHEMA_DIFF: &str = "diff/1";
pub const SCHEMA_EVENTS: &str = "events/1";

/// A window asked about and everything known about where it is.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WindowMatch {
    pub window: Window,
    pub location: Location,
    /// Where the layout says this container should be, when it is a tiled window.
    pub layout_rect: Option<Rect>,
    /// Parked off screen the way komorebi hides windows.
    pub hidden_position: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WorkspaceSummary {
    pub monitor_id: u64,
    pub name: String,
    pub showing: bool,
    pub windows: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StateData {
    pub at: Moment,
    pub origin: StateOrigin,
    pub focus: Option<Focus>,
    pub paused: bool,
    /// The monitors, with only the workspaces asked for (the showing ones by default).
    pub monitors: Vec<Monitor>,
    pub workspaces: Vec<WorkspaceSummary>,
    pub matches: Vec<WindowMatch>,
    pub violations: Vec<Violation>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ChangeView {
    pub text: String,
    #[serde(flatten)]
    pub change: Change,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DiffData {
    pub from: Moment,
    pub from_origin: StateOrigin,
    pub to: Moment,
    pub to_origin: StateOrigin,
    pub changes: Vec<ChangeView>,
    /// Violations present at `to` and not at `from`.
    pub violations_appeared: Vec<Violation>,
    /// Violations present at `from` and not at `to`.
    pub violations_resolved: Vec<Violation>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EventsData {
    pub events: Vec<EventView>,
    /// How many events matched, including any not returned.
    pub matching: usize,
    pub total: usize,
    /// The name komorebi knew this recorder by. When komorebi logs at DEBUG it names it for each
    /// notification pushed, which makes pairing events with handlers exact.
    pub subscriber: Option<String>,
}

/// Which windows, monitors and workspaces a state answer is about.
#[derive(Debug, Clone, Default)]
pub struct StateQuery {
    /// A window id, or text found in a window's application or title.
    pub window: Option<String>,
    pub monitor: Option<u64>,
    /// Include workspaces that are not showing.
    pub all: bool,
}

/// Which events an events answer includes.
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    pub from_mach_ns: Option<u64>,
    pub to_mach_ns: Option<u64>,
    pub classes: Vec<Class>,
    pub hide_echo: bool,
    pub name: Option<String>,
    pub pid: Option<i32>,
    pub window: Option<u32>,
    /// Most events to return; `None` for all.
    pub limit: Option<usize>,
    /// Only events after this sequence number.
    pub after_seq: Option<u64>,
}

/// An events answer and where to continue if it was cut short.
pub struct EventsAnswer {
    pub data: EventsData,
    pub next_cursor: Option<String>,
}

fn matches_window(text: &str, window: &Window) -> bool {
    if let Ok(id) = text.parse::<u64>()
        && window.id == id
    {
        return true;
    }

    let needle = text.to_lowercase();
    window.exe.to_lowercase().contains(&needle) || window.title.to_lowercase().contains(&needle)
}

impl Analysis {
    fn origin(&self, at: &StateAt, asked_mach_ns: u64) -> StateOrigin {
        match at.source {
            StateSource::Event { seq, recv_mach_ns } => StateOrigin {
                source: "event".to_string(),
                seq: Some(seq),
                state_id: at.state_id,
                age_ms: Some(asked_mach_ns.saturating_sub(recv_mach_ns) as f64 / 1e6),
            },
            StateSource::Start => StateOrigin {
                source: "start".to_string(),
                seq: None,
                state_id: None,
                age_ms: Some(
                    asked_mach_ns.saturating_sub(self.manifest.start.mach_ns) as f64 / 1e6,
                ),
            },
        }
    }

    /// The state at `mach_ns`, filtered to what `query` asks about.
    pub fn state_answer(&self, mach_ns: u64, query: &StateQuery) -> eyre::Result<StateData> {
        let found = self.state_at(mach_ns)?;
        let snapshot: &Snapshot = &found.snapshot;

        let matches: Vec<WindowMatch> = match &query.window {
            Some(text) => snapshot
                .windows()
                .into_iter()
                .filter(|(_, window)| matches_window(text, window))
                .map(|(location, window)| {
                    let monitor = snapshot
                        .monitors
                        .iter()
                        .find(|monitor| monitor.id == location.monitor_id);
                    let layout_rect = monitor
                        .and_then(|monitor| {
                            monitor
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.name == location.workspace)
                        })
                        .and_then(|workspace| match location.place {
                            Place::Container { index, .. } if workspace.monocle.is_none() => {
                                workspace.layout_rects.get(index).copied()
                            }
                            _ => None,
                        });
                    let hidden_position = window
                        .rect
                        .as_ref()
                        .zip(monitor)
                        .is_some_and(|(rect, monitor)| Snapshot::is_hidden_position(rect, monitor));
                    WindowMatch {
                        window: window.clone(),
                        location,
                        layout_rect,
                        hidden_position,
                    }
                })
                .collect(),
            None => Vec::new(),
        };
        if query.window.is_some() && matches.is_empty() {
            return Err(envelope::not_found(format!(
                "no window matches '{}' in the state at {}",
                query.window.as_deref().unwrap_or_default(),
                self.moment(mach_ns).timecode
            )));
        }

        let all_windows = snapshot.windows();
        let workspaces: Vec<WorkspaceSummary> = snapshot
            .monitors
            .iter()
            .flat_map(|monitor| {
                let all_windows = &all_windows;
                monitor
                    .workspaces
                    .iter()
                    .enumerate()
                    .map(move |(index, workspace)| WorkspaceSummary {
                        monitor_id: monitor.id,
                        name: workspace.name.clone(),
                        showing: index == monitor.focused,
                        windows: all_windows
                            .iter()
                            .filter(|(location, _)| {
                                location.monitor_id == monitor.id
                                    && location.workspace == workspace.name
                            })
                            .count(),
                    })
            })
            .collect();

        let wanted = |monitor: &Monitor, index: usize, name: &str| {
            query.all
                || index == monitor.focused
                || matches.iter().any(|found| {
                    found.location.monitor_id == monitor.id && found.location.workspace == name
                })
        };
        let monitors: Vec<Monitor> = snapshot
            .monitors
            .iter()
            .filter(|monitor| query.monitor.is_none_or(|id| id == monitor.id))
            .map(|monitor| {
                let mut kept = monitor.clone();
                kept.workspaces = monitor
                    .workspaces
                    .iter()
                    .enumerate()
                    .filter(|(index, workspace)| wanted(monitor, *index, &workspace.name))
                    .map(|(_, workspace)| workspace.clone())
                    .collect();
                kept
            })
            .collect();

        let mut violations = invariants::check(snapshot);
        if let Some(id) = query.monitor {
            violations.retain(|violation| violation.monitor_id.is_none_or(|monitor| monitor == id));
        }
        if !matches.is_empty() {
            violations.retain(|violation| {
                violation.window_ids.is_empty()
                    || violation
                        .window_ids
                        .iter()
                        .any(|id| matches.iter().any(|found| found.window.id == *id))
            });
        }

        Ok(StateData {
            at: self.moment(mach_ns),
            origin: self.origin(&found, mach_ns),
            focus: snapshot.focus(),
            paused: snapshot.paused,
            monitors,
            workspaces,
            matches,
            violations,
        })
    }

    /// What changed between the state at `from` and the state at `to`.
    pub fn diff_answer(&self, from_mach_ns: u64, to_mach_ns: u64) -> eyre::Result<DiffData> {
        let before = self.state_at(from_mach_ns)?;
        let after = self.state_at(to_mach_ns)?;
        let changes = diff::diff(&before.snapshot, &after.snapshot);

        let (was, now) = (
            invariants::check(&before.snapshot),
            invariants::check(&after.snapshot),
        );
        Ok(DiffData {
            from: self.moment(from_mach_ns),
            from_origin: self.origin(&before, from_mach_ns),
            to: self.moment(to_mach_ns),
            to_origin: self.origin(&after, to_mach_ns),
            changes: changes
                .into_iter()
                .map(|change| ChangeView {
                    text: change.describe(),
                    change,
                })
                .collect(),
            violations_appeared: now.iter().filter(|v| !was.contains(v)).cloned().collect(),
            violations_resolved: was.iter().filter(|v| !now.contains(v)).cloned().collect(),
        })
    }

    /// The events matching `query`, in order.
    #[must_use]
    pub fn events_answer(&self, query: &EventQuery) -> EventsAnswer {
        let all = self.events();
        let needle = query.name.as_deref().map(str::to_lowercase);
        let wanted: Vec<usize> = (0..all.len())
            .filter(|index| {
                let event = &all[*index];
                let class = self.pairing().classes[*index].class;
                query
                    .from_mach_ns
                    .is_none_or(|from| event.recv_mach_ns >= from)
                    && query.to_mach_ns.is_none_or(|to| event.recv_mach_ns <= to)
                    && (query.classes.is_empty() || query.classes.contains(&class))
                    && !(query.hide_echo && class == Class::Echo)
                    && needle
                        .as_ref()
                        .is_none_or(|needle| event.info.name.to_lowercase().contains(needle))
                    && query.pid.is_none_or(|pid| event.info.pid == Some(pid))
                    && query
                        .window
                        .is_none_or(|window| event.info.window_id == Some(window))
            })
            .collect();

        let remaining: Vec<usize> = wanted
            .iter()
            .copied()
            .filter(|index| query.after_seq.is_none_or(|after| all[*index].seq > after))
            .collect();
        let count = query
            .limit
            .map_or(remaining.len(), |limit| limit.min(remaining.len()));
        let events: Vec<EventView> = remaining[..count]
            .iter()
            .filter_map(|index| self.event_view(*index))
            .collect();
        let next_cursor = (count < remaining.len())
            .then(|| events.last().map(|view| view.seq.to_string()))
            .flatten();

        EventsAnswer {
            data: EventsData {
                events,
                matching: wanted.len(),
                total: all.len(),
                subscriber: self.pairing().subscriber.clone(),
            },
            next_cursor,
        }
    }
}

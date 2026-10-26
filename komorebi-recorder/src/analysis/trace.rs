//! One event, end to end: what ran, what it logged, what it changed and who changed it.
//!
//! komorebi sends the state *after* each notification, so what an event changed is the difference
//! between its state and the previous event's. That difference is not all the handler's doing:
//! background threads (the reaper, the workspace reconciliator) change state with no notification of
//! their own, and their changes show up in whichever event comes next. So every change is given an
//! **attribution** with the evidence for it and how sure it is, and a change nothing explains is
//! said to be unexplained rather than blamed on the handler.

use super::Analysis;
use super::answers::ChangeView;
use super::catalog;
use super::catalog::Confidence;
use super::diff;
use super::diff::Change;
use super::envelope;
use super::invariants;
use super::invariants::Violation;
use super::logparse::Line;
use super::timespec::Edge;
use super::timespec::Lookup;
use super::views::EventView;
use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;

pub const SCHEMA_TRACE: &str = "trace/1";

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SourceRef {
    pub location: String,
    pub function: String,
    pub confidence: Confidence,
}

/// A line of komorebi's log, with where it came from in the source.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TraceLine {
    pub line: u64,
    pub video_s: Option<f64>,
    pub level: Option<String>,
    /// The span chain, e.g. `process_event:focus_window`.
    pub spans: String,
    pub target: Option<String>,
    pub message: String,
    pub source: Option<SourceRef>,
    pub changes_state_silently: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Attribution {
    /// `handler`, `reaper`, `workspace-reconciliator` or `unexplained`.
    pub actor: String,
    /// `high`, `medium` or `low`.
    pub confidence: String,
    /// Log lines that support it.
    pub evidence: Vec<u64>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TracedChange {
    #[serde(flatten)]
    pub change: ChangeView,
    pub attribution: Attribution,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TraceData {
    pub event: EventView,
    /// What the handler logged, in order; empty if the log has no block for this event.
    pub handler: Vec<TraceLine>,
    /// Lines from other threads logged between the previous notification and this one: what else
    /// was going on.
    pub ambient: Vec<TraceLine>,
    pub changes: Vec<TracedChange>,
    pub violations_appeared: Vec<Violation>,
    pub violations_resolved: Vec<Violation>,
    /// Things worth knowing that are not data: a slow handler, errors, a missing log.
    pub notes: Vec<String>,
}

fn spans_of(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span| span.name.as_str())
        .collect::<Vec<_>>()
        .join(":")
}

/// Whether a change is the kind of thing this event's handler is expected to do.
fn expected_of_handler(event: &EventView, change: &Change) -> bool {
    use crate::analysis::events::Family;
    match event.info.family {
        // commands, monitor changes and anything unreadable can do anything
        Family::Command | Family::Monitor | Family::Unknown => true,
        Family::WindowManager => match event.info.name.as_str() {
            "FocusChange" => matches!(
                change,
                Change::Focus { .. }
                    | Change::WorkspaceFocus { .. }
                    | Change::WindowMoved { .. }
                    | Change::WindowRect { .. }
                    | Change::ContainerOrder { .. }
            ),
            "MoveStart" | "MoveEnd" | "ResizeStart" | "ResizeEnd" => matches!(
                change,
                Change::WindowRect { .. }
                    | Change::WindowMoved { .. }
                    | Change::ContainerOrder { .. }
                    | Change::Focus { .. }
            ),
            // showing, hiding and closing windows reshuffles everything around them
            _ => true,
        },
    }
}

impl Analysis {
    fn trace_line(&self, line: &Line) -> TraceLine {
        let source = match (&line.target, line.level) {
            (Some(target), Some(level)) => catalog::find(target, level, &line.message),
            _ => None,
        };
        let role = line.target.as_deref().map(catalog::role);
        TraceLine {
            line: line.number,
            video_s: line
                .wall_us
                .and_then(|wall_us| self.clock().wall_to_mach(wall_us))
                .map(|mach_ns| self.video_s(mach_ns)),
            level: line.level.map(|level| format!("{level:?}").to_uppercase()),
            spans: spans_of(line),
            target: line.target.clone(),
            message: line.message.clone(),
            source: source.map(|source| SourceRef {
                location: source.statement.location(),
                function: source.statement.qualified_function(),
                confidence: source.confidence,
            }),
            changes_state_silently: role.is_some_and(|role| role.changes_state_silently),
        }
    }

    /// Trace event `seq`.
    pub fn trace(&self, seq: u64) -> eyre::Result<TraceData> {
        let index = self
            .event_index(seq)
            .ok_or_else(|| envelope::not_found(format!("there is no event #{seq}")))?;
        let view = self
            .event_view(index)
            .ok_or_else(|| envelope::not_found(format!("there is no event #{seq}")))?;
        let event = &self.events()[index];
        let log = self.log();
        let pair = &self.pairing().pairs[index];
        let block = pair.block.and_then(|id| log.blocks.get(id));

        let handler: Vec<TraceLine> = block
            .map(|block| {
                block
                    .lines
                    .iter()
                    .filter_map(|number| log.lines.get(usize::try_from(number - 1).ok()?))
                    .map(|line| self.trace_line(line))
                    .collect()
            })
            .unwrap_or_default();

        // lines from other threads between the previous notification and this one
        let previous_recv = index
            .checked_sub(1)
            .map_or(0, |before| self.events()[before].recv_mach_ns);
        let handler_lines: std::collections::HashSet<u64> = log
            .blocks
            .iter()
            .flat_map(|block| block.lines.iter().copied())
            .collect();
        let ambient_lines: Vec<&Line> = log
            .lines
            .iter()
            .filter(|line| !line.continuation && !handler_lines.contains(&line.number))
            .filter(|line| {
                line.wall_us
                    .and_then(|wall_us| self.clock().wall_to_mach(wall_us))
                    .is_some_and(|mach_ns| mach_ns > previous_recv && mach_ns <= event.recv_mach_ns)
            })
            .collect();

        // what changed, and who did it
        let start = self.event_mach_ns(seq, Edge::Start)?;
        let (changes, appeared, resolved, mut notes) =
            if index == 0 && self.state_at(start).is_err() {
                (
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    vec![
                        "no state was recorded before this event, so what it changed is unknown"
                            .to_string(),
                    ],
                )
            } else if event.state_id.is_none() {
                (
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    vec!["this notification carried no state".to_string()],
                )
            } else {
                let before = self.state_at(start)?;
                let after = self.state_at(event.recv_mach_ns)?;
                let raw = diff::diff(&before.snapshot, &after.snapshot);
                let (was, now) = (
                    invariants::check(&before.snapshot),
                    invariants::check(&after.snapshot),
                );
                let traced = raw
                    .into_iter()
                    .map(|change| {
                        let attribution = self.attribute(&view, &change, &ambient_lines);
                        TracedChange {
                            change: ChangeView {
                                text: change.describe(),
                                change,
                            },
                            attribution,
                        }
                    })
                    .collect();
                (
                    traced,
                    now.iter().filter(|v| !was.contains(v)).cloned().collect(),
                    was.iter().filter(|v| !now.contains(v)).cloned().collect(),
                    Vec::new(),
                )
            };

        if let Some(block) = block {
            if let Some(ms) = pair.handler_ms
                && ms >= 100.0
            {
                notes.push(format!(
                    "the handler took at least {ms:.0} ms from its header to the notification: \
                     time spent in accessibility calls into other applications, or waiting for \
                     the lock held by another handler"
                ));
            }
            if block.errors + block.warnings > 0 {
                notes.push(format!(
                    "the handler logged {} error(s) and {} warning(s)",
                    block.errors, block.warnings
                ));
            }
        } else {
            notes.push(
                "no handler lines for this event were found in komorebi's log: it may not have \
                 been recorded, or komorebi may log this kind of event only at a more verbose level"
                    .to_string(),
            );
        }
        if view.class == super::classify::Class::Echo {
            notes.push(view.reason.clone());
        }

        Ok(TraceData {
            event: view,
            handler,
            ambient: ambient_lines
                .iter()
                .map(|line| self.trace_line(line))
                .collect(),
            changes,
            violations_appeared: appeared,
            violations_resolved: resolved,
            notes,
        })
    }

    /// Say who made `change`, from what the log shows happened around it.
    fn attribute(&self, event: &EventView, change: &Change, ambient: &[&Line]) -> Attribution {
        let lines_of = |target: &str, needle: &str| -> Vec<u64> {
            ambient
                .iter()
                .filter(|line| {
                    line.target.as_deref() == Some(target) && line.message.contains(needle)
                })
                .map(|line| line.number)
                .collect()
        };

        // a window removed that the reaper logged reaping, by id
        if let Change::WindowRemoved { window, .. } = change {
            let evidence = lines_of(
                "komorebi::reaper",
                &format!("reaping window: {}", window.id),
            );
            if !evidence.is_empty() {
                return Attribution {
                    actor: "reaper".to_string(),
                    confidence: "high".to_string(),
                    evidence,
                    reason: format!(
                        "the reaper logged reaping window {}: it removes windows that no longer \
                         exist, without sending a notification",
                        window.id
                    ),
                };
            }
        }

        let reconciling = lines_of("komorebi::workspace_reconciliator", "reconciliat");
        let moves = matches!(
            change,
            Change::WindowMoved { .. }
                | Change::WorkspaceFocus { .. }
                | Change::ContainerOrder { .. }
                | Change::WindowRect { .. }
        );
        let by_handler = expected_of_handler(event, change);

        if moves && !reconciling.is_empty() {
            return Attribution {
                actor: "workspace-reconciliator".to_string(),
                confidence: if by_handler { "low" } else { "medium" }.to_string(),
                evidence: reconciling,
                reason: if by_handler {
                    "the workspace reconciliator ran in this interval and could have done this, \
                     but so could the handler"
                        .to_string()
                } else {
                    "the workspace reconciliator ran in this interval, and this is not what this \
                     event's handler does"
                        .to_string()
                },
            };
        }

        if by_handler {
            let quality = event.handler.as_ref().map(|handler| handler.quality);
            return Attribution {
                actor: "handler".to_string(),
                confidence: if quality.is_some() { "high" } else { "medium" }.to_string(),
                evidence: event
                    .handler
                    .as_ref()
                    .map(|handler| vec![handler.header_line])
                    .unwrap_or_default(),
                reason: format!(
                    "{} is the kind of change {} makes",
                    change.kind(),
                    event.info.name
                ),
            };
        }

        Attribution {
            actor: "unexplained".to_string(),
            confidence: "low".to_string(),
            evidence: Vec::new(),
            reason: format!(
                "{} is not something {} does, and no background activity in the log accounts for \
                 it: komorebi changed state with no notification, or the log is at a level that \
                 does not show it",
                change.kind(),
                event.info.name
            ),
        }
    }
}

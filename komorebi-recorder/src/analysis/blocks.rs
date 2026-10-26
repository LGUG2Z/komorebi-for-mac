//! Which lines of komorebi's log belong to which event.
//!
//! komorebi handles one event or command at a time, under one lock, and everything it logs while
//! doing so carries the handler's span (`process_event:` or `process_command:`). That makes a
//! handler's output recoverable as a **block**: the header line (`processing event: ...`) and the
//! lines after it that share the span. Lines logged from other threads (the border manager, the
//! reaper, the reconcilers) carry no handler span; they are *ambient* and can only be related to a
//! handler by when they happened.
//!
//! Subscriber notifications are sent *after* the handler body, so an event's receive time marks the
//! **end** of its handler, not its start. [`pair`] ties each recorded event to the block that
//! produced it, and says how sure it is:
//!
//! - [`Quality::Pushed`]: komorebi logged `pushed notification to subscriber: <us>` (a DEBUG line)
//!   just before sending, which is exact.
//! - [`Quality::Header`]: matched by name, process and notification to the last header before the
//!   event was received, after the previous event.
//! - [`Quality::None`]: no block found. Events whose header is only logged at TRACE (the manual
//!   `Show` hacks) are like this, as are events recorded while komorebi's log was off.
//!
//! What cannot be known from the log is when komorebi *dequeued* the event: the time spent waiting
//! for the lock or for the accessibility calls made before the header is invisible.

use super::events::EventRecord;
use super::events::Family;
use super::logparse::Level;
use super::logparse::Line;
use crate::clock::ClockMap;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BlockKind {
    Event,
    Command,
}

/// A line komorebi logged when it handed a notification to a subscriber.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Push {
    pub subscriber: String,
    pub line: u64,
    pub mach_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Block {
    pub id: usize,
    pub kind: BlockKind,
    /// `MoveEnd`, `FocusWorkspaceNumber`, ...
    pub name: String,
    pub pid: Option<i32>,
    pub notification: Option<String>,
    pub header_line: u64,
    /// When the header was logged: the earliest known moment of the handler.
    pub header_mach_ns: u64,
    pub last_line: u64,
    /// When the handler's last line was logged.
    pub end_mach_ns: u64,
    /// Line numbers of everything the handler logged, header first.
    pub lines: Vec<u64>,
    /// The line saying a command finished (`processed command:`); commands only.
    pub finished_line: Option<u64>,
    pub pushes: Vec<Push>,
    pub errors: u32,
    pub warnings: u32,
}

impl Block {
    /// Milliseconds from the header to the handler's last line.
    #[must_use]
    pub fn logged_ms(&self) -> f64 {
        (self.end_mach_ns.saturating_sub(self.header_mach_ns)) as f64 / 1e6
    }
}

const EVENT_HEADER: &str = "processing event: ";
const COMMAND_HEADER: &str = "processing command: ";
const COMMAND_FINISHED: &str = "processed command: ";
const PUSHED: &str = "pushed notification to subscriber: ";

/// `FocusChange for process 79469 with notification AXApplicationActivated`
fn parse_event_header(rest: &str) -> (String, Option<i32>, Option<String>) {
    let (name, tail) = rest.split_once(" for process ").unwrap_or((rest, ""));
    let (pid, notification) = tail.split_once(" with notification ").unwrap_or((tail, ""));
    (
        name.trim().to_string(),
        pid.trim().parse().ok(),
        Some(notification.trim().to_string()).filter(|text| !text.is_empty()),
    )
}

/// Group the handler lines of a log into blocks. `lines` are in file order.
#[must_use]
pub fn build(lines: &[Line], clock: &ClockMap) -> Vec<Block> {
    let mach = |line: &Line| line.wall_us.and_then(|wall_us| clock.wall_to_mach(wall_us));

    let mut blocks: Vec<Block> = Vec::new();
    let mut open: Option<usize> = None;

    for line in lines.iter().filter(|line| !line.continuation) {
        let Some(time) = mach(line) else {
            continue;
        };

        match line.root_span() {
            Some("process_event") if line.message.starts_with(EVENT_HEADER) => {
                let (name, pid, notification) =
                    parse_event_header(&line.message[EVENT_HEADER.len()..]);
                blocks.push(new_block(
                    blocks.len(),
                    BlockKind::Event,
                    name,
                    pid,
                    notification,
                    line.number,
                    time,
                ));
                open = Some(blocks.len() - 1);
            }
            Some("process_command") if line.message.starts_with(COMMAND_HEADER) => {
                let name = line.message[COMMAND_HEADER.len()..].trim().to_string();
                blocks.push(new_block(
                    blocks.len(),
                    BlockKind::Command,
                    name,
                    None,
                    None,
                    line.number,
                    time,
                ));
                open = Some(blocks.len() - 1);
            }
            Some(root @ ("process_event" | "process_command")) => {
                let Some(index) = open else {
                    continue;
                };
                let block = &mut blocks[index];
                let belongs = matches!(
                    (root, block.kind),
                    ("process_event", BlockKind::Event) | ("process_command", BlockKind::Command)
                );
                if !belongs {
                    continue;
                }

                block.lines.push(line.number);
                block.last_line = line.number;
                block.end_mach_ns = time;
                match line.level {
                    Some(Level::Error) => block.errors += 1,
                    Some(Level::Warn) => block.warnings += 1,
                    _ => {}
                }

                if let Some(subscriber) = line.message.strip_prefix(PUSHED) {
                    block.pushes.push(Push {
                        subscriber: subscriber.trim().to_string(),
                        line: line.number,
                        mach_ns: time,
                    });
                }

                if root == "process_command" && line.message.starts_with(COMMAND_FINISHED) {
                    block.finished_line = Some(line.number);
                    open = None;
                }
            }
            _ => {}
        }
    }

    blocks
}

fn new_block(
    id: usize,
    kind: BlockKind,
    name: String,
    pid: Option<i32>,
    notification: Option<String>,
    line: u64,
    time: u64,
) -> Block {
    Block {
        id,
        kind,
        name,
        pid,
        notification,
        header_line: line,
        header_mach_ns: time,
        last_line: line,
        end_mach_ns: time,
        lines: vec![line],
        finished_line: None,
        pushes: Vec::new(),
        errors: 0,
        warnings: 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    /// komorebi logged handing the notification to this subscriber.
    Pushed,
    /// Matched by name, process and notification.
    Header,
    /// No block was found.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Pair {
    pub seq: u64,
    pub block: Option<usize>,
    pub quality: Quality,
    /// The `pushed notification` line for this event, if there was one.
    pub pushed_line: Option<u64>,
    /// Milliseconds from the handler's header to the event being received: how long the handler
    /// took, at least.
    pub handler_ms: Option<f64>,
}

/// How far a log time may be from the recorder's own clock reading and still be taken to be the
/// same moment. The two are mapped through the clock anchors, which are accurate to about a
/// millisecond; the margin is for scheduling in between.
const SLACK_NS: u64 = 25_000_000;

/// How close a `pushed notification` line must be before an event's receipt.
const PUSH_WINDOW_NS: u64 = 100_000_000;

/// The subscriber name the recorder registered as, found from which `pushed notification` lines
/// line up with the recorded events.
#[must_use]
pub fn infer_subscriber(blocks: &[Block], events: &[EventRecord]) -> Option<String> {
    let mut pushes: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for push in blocks.iter().flat_map(|block| &block.pushes) {
        pushes
            .entry(push.subscriber.as_str())
            .or_default()
            .push(push.mach_ns);
    }

    let score = |times: &[u64]| {
        events
            .iter()
            .filter(|event| {
                times.iter().any(|time| {
                    *time <= event.recv_mach_ns + SLACK_NS
                        && event.recv_mach_ns <= *time + PUSH_WINDOW_NS
                })
            })
            .count()
    };

    pushes
        .into_iter()
        .map(|(name, times)| {
            // our own subscriber names start with this; prefer them on a tie
            let ours = name.starts_with("komorebi-recorder-");
            (score(&times), ours, name)
        })
        .filter(|(score, _, _)| *score > 0)
        .max_by_key(|(score, ours, _)| (*score, *ours))
        .map(|(_, _, name)| name.to_string())
}

/// The line where komorebi handed `block`'s notification to `subscriber`, if the log has it.
fn push_for<'a>(
    block: &'a Block,
    subscriber: Option<&str>,
    recv_mach_ns: u64,
    upper: u64,
) -> Option<&'a Push> {
    let name = subscriber?;
    block
        .pushes
        .iter()
        .filter(|push| push.subscriber == name)
        .filter(|push| push.mach_ns <= upper && recv_mach_ns <= push.mach_ns + PUSH_WINDOW_NS)
        .max_by_key(|push| push.mach_ns)
}

/// Tie each event to the block that produced it. `events` must be in receipt order.
#[must_use]
pub fn pair(events: &[EventRecord], blocks: &[Block], subscriber: Option<&str>) -> Vec<Pair> {
    let mut used = vec![false; blocks.len()];
    let mut previous_recv = 0_u64;
    let mut pairs = Vec::with_capacity(events.len());

    for event in events {
        let upper = event.recv_mach_ns.saturating_add(SLACK_NS);
        let lower = previous_recv.saturating_sub(SLACK_NS);

        let wanted = match event.info.family {
            Family::WindowManager => Some(BlockKind::Event),
            Family::Command => Some(BlockKind::Command),
            Family::Monitor | Family::Unknown => None,
        };

        let candidate = wanted.and_then(|kind| {
            blocks
                .iter()
                .filter(|block| {
                    !used[block.id]
                        && block.kind == kind
                        && block.name == event.info.name
                        && block.header_mach_ns > lower
                        && block.header_mach_ns <= upper
                        && match (&block.pid, &event.info.pid) {
                            (Some(a), Some(b)) => a == b,
                            _ => true,
                        }
                        && match (&block.notification, &event.info.notification) {
                            (Some(a), Some(b)) => a == b,
                            _ => true,
                        }
                })
                // the handler that finished last before the notification
                .max_by_key(|block| block.header_mach_ns)
        });

        let pair = if let Some(block) = candidate {
            used[block.id] = true;
            let push = push_for(block, subscriber, event.recv_mach_ns, upper);
            Pair {
                seq: event.seq,
                block: Some(block.id),
                quality: if push.is_some() {
                    Quality::Pushed
                } else {
                    Quality::Header
                },
                pushed_line: push.map(|push| push.line),
                handler_ms: Some(
                    event.recv_mach_ns.saturating_sub(block.header_mach_ns) as f64 / 1e6,
                ),
            }
        } else {
            Pair {
                seq: event.seq,
                block: None,
                quality: Quality::None,
                pushed_line: None,
                handler_ms: None,
            }
        };

        pairs.push(pair);
        previous_recv = event.recv_mach_ns;
    }

    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::events::describe;
    use crate::analysis::logparse;
    use crate::clock::Anchor;
    use crate::testkit;
    use serde_json::json;

    /// A clock where wall time is mach time plus the testkit's constant.
    fn clock() -> ClockMap {
        ClockMap::new(vec![
            Anchor {
                mach_ns: testkit::mach_ns(-1.0),
                wall_us: testkit::wall_us(-1.0),
            },
            Anchor {
                mach_ns: testkit::mach_ns(60.0),
                wall_us: testkit::wall_us(60.0),
            },
        ])
    }

    /// Log text from `(video_s, level, text-after-level)` lines, like komorebi's.
    fn log(lines: &[(f64, &str, &str)]) -> Vec<Line> {
        logparse::parse(testkit::log_text(lines).as_bytes())
    }

    fn event(seq: u64, video_s: f64, value: serde_json::Value) -> EventRecord {
        EventRecord {
            seq,
            recv_mach_ns: testkit::mach_ns(video_s),
            recv_wall_us: testkit::wall_us(video_s),
            state_id: None,
            info: describe(&value),
            parse_error: None,
        }
    }

    fn move_end(pid: i64) -> serde_json::Value {
        json!({"type": "MoveEnd", "content": [{"type": "Accessibility", "content": "AXWindowMoved"}, pid, 7]})
    }

    const HEADER: &str = "process_event: komorebi::process_event: processing event: MoveEnd for process 10 with notification AXWindowMoved";

    #[test]
    fn a_handlers_lines_are_everything_with_its_span_until_the_next_header() {
        let lines = log(&[
            (1.0, "INFO", HEADER),
            (
                1.1,
                "INFO",
                "process_event:focus_window{idx=0}: komorebi::container: focusing window",
            ),
            (
                1.15,
                "INFO",
                "komorebi::border_manager: invalidating border observer",
            ),
            (
                1.2,
                "WARN",
                "process_event:update_focused_workspace: komorebi::workspace: failed to set window position: AXError::InvalidUIElement",
            ),
            (
                2.0,
                "INFO",
                "process_event: komorebi::process_event: processing event: FocusChange for process 11 with notification AXApplicationActivated",
            ),
        ]);

        let blocks = build(&lines, &clock());
        assert_eq!(blocks.len(), 2);

        let first = &blocks[0];
        assert_eq!(first.kind, BlockKind::Event);
        assert_eq!(first.name, "MoveEnd");
        assert_eq!(first.pid, Some(10));
        assert_eq!(first.notification.as_deref(), Some("AXWindowMoved"));
        // the border manager's line is ambient: logged from another thread, no handler span
        assert_eq!(first.lines, [1, 2, 4]);
        assert_eq!(first.warnings, 1);
        assert!((first.logged_ms() - 200.0).abs() < 1.0);

        assert_eq!(blocks[1].name, "FocusChange");
        assert_eq!(blocks[1].lines, [5]);
    }

    #[test]
    fn a_command_block_ends_at_its_processed_line() {
        let lines = log(&[
            (
                1.0,
                "INFO",
                "process_command: komorebi::process_command: processing command: FocusWorkspaceNumber",
            ),
            (
                1.1,
                "INFO",
                "process_command:focus_workspace{idx=0}: komorebi::window_manager: focusing workspace",
            ),
            (
                1.3,
                "INFO",
                "process_command: komorebi::process_command: processed command: FocusWorkspaceNumber",
            ),
            // after the command ended: ambient again
            (
                1.4,
                "INFO",
                "process_command:something: komorebi::x: late line",
            ),
        ]);

        let blocks = build(&lines, &clock());
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Command);
        assert_eq!(blocks[0].name, "FocusWorkspaceNumber");
        assert_eq!(blocks[0].finished_line, Some(3));
        assert_eq!(blocks[0].lines, [1, 2, 3]);
    }

    #[test]
    fn handler_lines_before_any_header_belong_to_no_block() {
        let lines = log(&[
            (
                0.5,
                "DEBUG",
                "process_event: komorebi::process_event: ignoring event as window should not be managed: X",
            ),
            (1.0, "INFO", HEADER),
        ]);

        let blocks = build(&lines, &clock());
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines, [2]);
    }

    #[test]
    fn an_event_pairs_with_the_last_matching_header_before_it_was_received() {
        let lines = log(&[(1.0, "INFO", HEADER), (3.0, "INFO", HEADER)]);
        let blocks = build(&lines, &clock());
        // received at 3.25s: the handler that began at 3.0s sent it; the one at 1.0s never did
        let events = vec![event(1, 3.25, move_end(10))];

        let pairs = pair(&events, &blocks, None);
        assert_eq!(pairs[0].block, Some(1));
        assert_eq!(pairs[0].quality, Quality::Header);
        assert!((pairs[0].handler_ms.expect("ms") - 250.0).abs() < 1.0);
    }

    #[test]
    fn an_event_is_not_paired_across_another_event() {
        let lines = log(&[(1.0, "INFO", HEADER)]);
        let blocks = build(&lines, &clock());
        let events = vec![
            event(1, 1.1, move_end(10)),
            // a second event of the same kind with no handler logged: it must not steal block 0
            event(2, 5.0, move_end(10)),
        ];

        let pairs = pair(&events, &blocks, None);
        assert_eq!(pairs[0].block, Some(0));
        assert_eq!(pairs[1].block, None);
        assert_eq!(pairs[1].quality, Quality::None);
    }

    #[test]
    fn name_process_and_notification_must_agree() {
        let lines = log(&[(1.0, "INFO", HEADER)]);
        let blocks = build(&lines, &clock());

        let other_process = pair(&[event(1, 1.1, move_end(99))], &blocks, None);
        assert_eq!(other_process[0].block, None);

        let other_name = pair(
            &[event(
                1,
                1.1,
                json!({"type": "ResizeEnd", "content": [{"type": "Accessibility", "content": "AXWindowMoved"}, 10, 7]}),
            )],
            &blocks,
            None,
        );
        assert_eq!(other_name[0].block, None);
    }

    #[test]
    fn a_pushed_line_makes_the_pairing_exact_and_names_our_subscriber() {
        let lines = log(&[
            (1.0, "INFO", HEADER),
            (
                1.2,
                "DEBUG",
                "process_event: komorebi::notify: pushed notification to subscriber: komorebi-bar-x",
            ),
            (
                1.2001,
                "DEBUG",
                "process_event: komorebi::notify: pushed notification to subscriber: komorebi-recorder-4242",
            ),
        ]);
        let blocks = build(&lines, &clock());
        let events = vec![event(1, 1.2005, move_end(10))];

        assert_eq!(
            infer_subscriber(&blocks, &events).as_deref(),
            Some("komorebi-recorder-4242")
        );

        let pairs = pair(&events, &blocks, Some("komorebi-recorder-4242"));
        assert_eq!(pairs[0].quality, Quality::Pushed);
        assert_eq!(pairs[0].pushed_line, Some(3));
    }

    #[test]
    fn commands_and_monitor_events_pair_by_family() {
        let lines = log(&[
            (
                1.0,
                "INFO",
                "process_command: komorebi::process_command: processing command: Retile",
            ),
            (
                1.2,
                "INFO",
                "process_command: komorebi::process_command: processed command: Retile",
            ),
        ]);
        let blocks = build(&lines, &clock());
        let events = vec![
            event(1, 1.1, json!({"type": "Retile"})),
            event(2, 3.0, json!({"DisplayConnectionChange": 1})),
        ];

        let pairs = pair(&events, &blocks, None);
        assert_eq!(pairs[0].block, Some(0));
        assert_eq!(pairs[1].block, None);
    }
}

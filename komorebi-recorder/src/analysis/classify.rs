//! What kind of thing each event was, and which ones are noise.
//!
//! A recording of komorebi at work is mostly events nobody asked for: when komorebi moves a window
//! the OS reports the move back, so a single command is followed by a burst of `MoveEnd`
//! notifications for the windows it just placed. Reading those as if the user had dragged windows
//! around sends an investigation the wrong way, so events are classified, with the reason.
//!
//! The classification is a judgement made from the event, its neighbours and whether the state
//! changed, and says so in `reason`. It never hides an event; it labels it.

use super::events::EventRecord;
use super::events::Family;
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Class {
    /// Sent to komorebi by `komorebic` or a hotkey daemon.
    Command,
    /// The OS reported a window being moved or resized, not caused by komorebi.
    Drag,
    /// Which application or window has focus changed.
    Focus,
    /// A window or application appeared, went away, was minimized or restored.
    Lifecycle,
    /// The OS reporting back a move or resize komorebi itself had just made.
    Echo,
    /// komorebi's own synthetic show events.
    Manual,
    /// A display was connected, disconnected or resized.
    Monitor,
    /// Screen lock, unlock or a space change.
    Lock,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Classification {
    pub class: Class,
    pub reason: String,
}

/// How long after komorebi acts the OS may still be reporting the windows it moved.
const ECHO_WINDOW_NS: u64 = 1_500_000_000;

const GEOMETRY_EVENTS: [&str; 4] = ["MoveStart", "MoveEnd", "ResizeStart", "ResizeEnd"];

/// Commands that ask komorebi something or register a listener rather than change anything.
fn is_bookkeeping(name: &str) -> bool {
    matches!(
        name,
        "State"
            | "GlobalState"
            | "AddSubscriberSocket"
            | "AddSubscriberSocketWithOptions"
            | "RemoveSubscriberSocket"
    ) || name.starts_with("Query")
}

fn class_of(event: &EventRecord) -> Classification {
    let (class, reason) = match event.info.family {
        Family::Command if is_bookkeeping(&event.info.name) => (
            Class::Other,
            "a query or subscription, not a change: usually this recorder talking to komorebi",
        ),
        Family::Command => (Class::Command, "a command sent to komorebi"),
        Family::Monitor => (Class::Monitor, "a display changed"),
        Family::Unknown => (Class::Other, "the notification could not be read"),
        Family::WindowManager => {
            if event
                .info
                .notification
                .as_deref()
                .is_some_and(|notification| notification.starts_with("ShowOn"))
            {
                (Class::Manual, "komorebi's own synthetic show event")
            } else {
                match event.info.name.as_str() {
                    "FocusChange" => (Class::Focus, "focus moved"),
                    "Show" | "Destroy" | "Minimize" | "Restore" | "Manage" | "Unmanage" => {
                        (Class::Lifecycle, "a window or application changed")
                    }
                    name if GEOMETRY_EVENTS.contains(&name) => (
                        Class::Drag,
                        "a window was moved or resized by something other than komorebi",
                    ),
                    "ScreenLock" | "ScreenUnlock" | "SpaceChange" => {
                        (Class::Lock, "the screen locked or the space changed")
                    }
                    _ => (Class::Other, "not a known kind of event"),
                }
            }
        }
    };

    Classification {
        class,
        reason: reason.to_string(),
    }
}

/// Classify every event. `events` must be in receipt order.
#[must_use]
pub fn classify(events: &[EventRecord]) -> Vec<Classification> {
    let mut classes: Vec<Classification> = events.iter().map(class_of).collect();

    for index in 0..events.len() {
        let event = &events[index];
        if classes[index].class != Class::Drag {
            continue;
        }

        // a move or resize that changed nothing, soon after komorebi acted, is komorebi's own
        let unchanged =
            index > 0 && event.state_id.is_some() && event.state_id == events[index - 1].state_id;
        if !unchanged {
            continue;
        }

        let acted = (0..index).rev().find_map(|earlier| {
            let gap = event
                .recv_mach_ns
                .saturating_sub(events[earlier].recv_mach_ns);
            if gap > ECHO_WINDOW_NS {
                return Some(None);
            }

            matches!(
                classes[earlier].class,
                Class::Command | Class::Focus | Class::Lifecycle | Class::Monitor | Class::Lock
            )
            .then(|| Some((earlier, gap)))
        });

        if let Some(Some((earlier, gap))) = acted {
            classes[index] = Classification {
                class: Class::Echo,
                reason: format!(
                    "state unchanged, {:.0} ms after komorebi handled {} (#{}): the OS reporting back windows komorebi moved",
                    gap as f64 / 1e6,
                    events[earlier].info.summary(),
                    events[earlier].seq
                ),
            };
        }
    }

    classes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::events::describe;
    use serde_json::json;

    fn event(seq: u64, at_ms: u64, state_id: Option<u64>, value: serde_json::Value) -> EventRecord {
        EventRecord {
            seq,
            recv_mach_ns: at_ms * 1_000_000,
            recv_wall_us: 0,
            state_id,
            info: describe(&value),
            parse_error: None,
        }
    }

    fn command(seq: u64, at_ms: u64, state: u64) -> EventRecord {
        event(
            seq,
            at_ms,
            Some(state),
            json!({"type": "FocusWorkspaceNumber", "content": 1}),
        )
    }

    fn move_end(seq: u64, at_ms: u64, state: u64) -> EventRecord {
        event(
            seq,
            at_ms,
            Some(state),
            json!({"type": "MoveEnd", "content": [{"type": "Accessibility", "content": "AXWindowMoved"}, 10, 7]}),
        )
    }

    fn classes(events: &[EventRecord]) -> Vec<Class> {
        classify(events).iter().map(|c| c.class).collect()
    }

    #[test]
    fn each_family_and_name_has_a_class() {
        let events = vec![
            command(1, 0, 1),
            event(2, 10, Some(2), json!({"DisplayConnectionChange": 1})),
            event(
                3,
                20,
                Some(3),
                json!({"type": "FocusChange", "content": [{"type": "Accessibility", "content": "AXApplicationActivated"}, 1, null]}),
            ),
            event(
                4,
                30,
                Some(4),
                json!({"type": "Show", "content": [{"type": "Accessibility", "content": "AXWindowCreated"}, 1]}),
            ),
            event(
                5,
                40,
                Some(5),
                json!({"type": "Show", "content": [{"type": "Manual", "content": "ShowOnInputEvent"}, 1]}),
            ),
            event(
                6,
                50,
                Some(6),
                json!({"type": "ScreenLock", "content": [{"type": "Accessibility", "content": "x"}, 1]}),
            ),
        ];

        assert_eq!(
            classes(&events),
            [
                Class::Command,
                Class::Monitor,
                Class::Focus,
                Class::Lifecycle,
                Class::Manual,
                Class::Lock
            ]
        );
    }

    #[test]
    fn queries_and_subscriptions_are_not_commands() {
        let events = vec![
            event(
                1,
                0,
                Some(1),
                json!({"type": "AddSubscriberSocketWithOptions", "content": ["x", {}]}),
            ),
            event(2, 10, Some(1), json!({"type": "State"})),
            event(3, 20, Some(1), json!({"type": "QueryFocusedWorkspaceName"})),
        ];
        assert_eq!(classes(&events), [Class::Other, Class::Other, Class::Other]);
    }

    #[test]
    fn a_move_that_changed_the_state_is_a_real_move() {
        let events = vec![command(1, 0, 1), move_end(2, 200, 2)];
        assert_eq!(classes(&events), [Class::Command, Class::Drag]);
    }

    #[test]
    fn moves_that_change_nothing_right_after_a_command_are_echoes_of_it() {
        // the burst seen after FocusWorkspaceNumber in a real log: one MoveEnd per window
        let events = vec![
            command(1, 0, 1),
            move_end(2, 80, 1),
            move_end(3, 120, 1),
            move_end(4, 160, 1),
        ];

        let found = classify(&events);
        assert_eq!(found[1].class, Class::Echo);
        assert_eq!(found[3].class, Class::Echo);
        assert!(
            found[1].reason.contains("FocusWorkspaceNumber"),
            "{}",
            found[1].reason
        );
        assert!(found[1].reason.contains("#1"));
    }

    #[test]
    fn an_unchanged_move_long_after_anything_is_not_called_an_echo() {
        let events = vec![command(1, 0, 1), move_end(2, 10_000, 1)];
        assert_eq!(classes(&events), [Class::Command, Class::Drag]);
    }

    #[test]
    fn an_unchanged_move_with_no_state_is_not_called_an_echo() {
        let events = vec![command(1, 0, 1), move_end(2, 100, 1)];
        let mut without = events.clone();
        without[1].state_id = None;
        assert_eq!(classes(&without)[1], Class::Drag);
        assert_eq!(classes(&events)[1], Class::Echo);
    }
}

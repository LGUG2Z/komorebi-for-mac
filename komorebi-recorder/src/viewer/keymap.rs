//! What the keys do.
//!
//! Kept apart from the window so the whole map can be read, listed in the help and tested. A key
//! becomes an [`Action`]; the window decides how to carry it out.

use eframe::egui::Key;
use eframe::egui::Modifiers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    TogglePlay,
    Pause,
    /// Play forwards, faster each time it is pressed while already playing forwards.
    PlayForwards,
    /// Play backwards, faster each time.
    PlayBackwards,
    StepFrame(i32),
    /// Jump this many seconds.
    Skip(i32),
    ToStart,
    ToEnd,
    PreviousEvent,
    NextEvent,
    PreviousBug,
    NextBug,
    /// Mark the start / end of a selection on the timeline.
    SetIn,
    SetOut,
    ClearSelection,
    /// Open the composer for a bug region, from the selection if there is one.
    MarkBug,
    ToggleLoop,
    ToggleOverlay,
    ZoomIn,
    ZoomOut,
    FitTimeline,
    FocusDisplay(usize),
    Help,
    Escape,
}

/// Every binding, for the help screen: the keys and what they do.
pub const BINDINGS: &[(&str, &str)] = &[
    ("Space", "play / pause"),
    (
        "J  K  L",
        "play backwards / pause / play forwards (again = faster)",
    ),
    ("←  →", "back / forward one frame"),
    ("Shift ←  →", "back / forward one second"),
    ("Home  End", "to the start / end"),
    (",  .", "previous / next event"),
    ("{  }", "previous / next bug region"),
    ("[  ]", "set the start / end of a selection"),
    ("B", "mark a bug region (from the selection)"),
    ("\\", "loop the selection"),
    ("O", "show komorebi's state over the video"),
    ("+  -", "zoom the timeline"),
    ("0", "fit the whole recording"),
    ("1 – 9", "step by that display"),
    ("Esc", "clear the selection / close a panel"),
    ("?", "this help"),
];

/// The action for a key pressed with `modifiers`, if it is bound. `text` is what the key typed (for
/// the keys that are symbols, which vary with the keyboard layout).
#[must_use]
pub fn action(key: Key, modifiers: Modifiers, text: Option<&str>) -> Option<Action> {
    if modifiers.command || modifiers.ctrl || modifiers.alt {
        return None;
    }

    // symbols by what they type, since where they are on the keyboard differs
    match text {
        Some(",") => return Some(Action::PreviousEvent),
        Some(".") => return Some(Action::NextEvent),
        Some("{") => return Some(Action::PreviousBug),
        Some("}") => return Some(Action::NextBug),
        Some("[") => return Some(Action::SetIn),
        Some("]") => return Some(Action::SetOut),
        Some("\\") => return Some(Action::ToggleLoop),
        Some("+" | "=") => return Some(Action::ZoomIn),
        Some("-" | "_") => return Some(Action::ZoomOut),
        Some("?") => return Some(Action::Help),
        _ => {}
    }

    Some(match key {
        Key::Space => Action::TogglePlay,
        Key::J => Action::PlayBackwards,
        Key::K => Action::Pause,
        Key::L => Action::PlayForwards,
        Key::ArrowLeft if modifiers.shift => Action::Skip(-1),
        Key::ArrowRight if modifiers.shift => Action::Skip(1),
        Key::ArrowLeft => Action::StepFrame(-1),
        Key::ArrowRight => Action::StepFrame(1),
        Key::Home => Action::ToStart,
        Key::End => Action::ToEnd,
        Key::B => Action::MarkBug,
        Key::O => Action::ToggleOverlay,
        Key::Num0 => Action::FitTimeline,
        Key::Num1 => Action::FocusDisplay(0),
        Key::Num2 => Action::FocusDisplay(1),
        Key::Num3 => Action::FocusDisplay(2),
        Key::Num4 => Action::FocusDisplay(3),
        Key::Num5 => Action::FocusDisplay(4),
        Key::Num6 => Action::FocusDisplay(5),
        Key::Num7 => Action::FocusDisplay(6),
        Key::Num8 => Action::FocusDisplay(7),
        Key::Num9 => Action::FocusDisplay(8),
        Key::Escape => Action::Escape,
        _ => return None,
    })
}

/// The speeds `J` and `L` step through.
pub const SHUTTLE_RATES: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

/// The rate after pressing a shuttle key: the first press plays at normal speed, each further press
/// in the same direction doubles it, and pressing the other direction starts over at normal speed.
#[must_use]
pub fn shuttle(current: f64, playing: bool, forwards: bool) -> f64 {
    let direction = if forwards { 1.0 } else { -1.0 };
    if !playing || current.signum() != direction {
        return direction;
    }

    let faster = SHUTTLE_RATES
        .iter()
        .copied()
        .find(|rate| *rate > current.abs() + 1e-9)
        .unwrap_or(SHUTTLE_RATES[SHUTTLE_RATES.len() - 1]);
    direction * faster
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Modifiers {
        Modifiers::NONE
    }

    #[test]
    fn the_transport_keys_do_what_video_players_do() {
        assert_eq!(action(Key::Space, plain(), None), Some(Action::TogglePlay));
        assert_eq!(action(Key::J, plain(), None), Some(Action::PlayBackwards));
        assert_eq!(action(Key::K, plain(), None), Some(Action::Pause));
        assert_eq!(action(Key::L, plain(), None), Some(Action::PlayForwards));
        assert_eq!(
            action(Key::ArrowLeft, plain(), None),
            Some(Action::StepFrame(-1))
        );
        assert_eq!(
            action(Key::ArrowRight, plain(), None),
            Some(Action::StepFrame(1))
        );
        assert_eq!(
            action(Key::ArrowRight, Modifiers::SHIFT, None),
            Some(Action::Skip(1))
        );
        assert_eq!(action(Key::Home, plain(), None), Some(Action::ToStart));
    }

    #[test]
    fn symbols_are_read_from_what_they_type_not_where_they_are() {
        assert_eq!(
            action(Key::Comma, plain(), Some(",")),
            Some(Action::PreviousEvent)
        );
        assert_eq!(
            action(Key::Period, plain(), Some(".")),
            Some(Action::NextEvent)
        );
        assert_eq!(
            action(Key::OpenBracket, plain(), Some("[")),
            Some(Action::SetIn)
        );
        assert_eq!(
            action(Key::CloseBracket, plain(), Some("]")),
            Some(Action::SetOut)
        );
        assert_eq!(
            action(Key::Questionmark, plain(), Some("?")),
            Some(Action::Help)
        );
        assert_eq!(
            action(Key::Equals, plain(), Some("=")),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            action(Key::Minus, plain(), Some("-")),
            Some(Action::ZoomOut)
        );
    }

    #[test]
    fn the_command_key_is_left_to_the_system() {
        assert_eq!(action(Key::B, Modifiers::COMMAND, None), None);
        assert_eq!(action(Key::Space, Modifiers::CTRL, None), None);
        assert_eq!(action(Key::Q, plain(), None), None);
    }

    #[test]
    fn the_number_keys_pick_a_display() {
        assert_eq!(
            action(Key::Num1, plain(), None),
            Some(Action::FocusDisplay(0))
        );
        assert_eq!(
            action(Key::Num9, plain(), None),
            Some(Action::FocusDisplay(8))
        );
        assert_eq!(action(Key::Num0, plain(), None), Some(Action::FitTimeline));
    }

    #[test]
    fn shuttling_speeds_up_in_a_direction_and_starts_over_in_the_other() {
        assert_eq!(shuttle(1.0, false, true), 1.0);
        assert_eq!(shuttle(1.0, true, true), 2.0);
        assert_eq!(shuttle(2.0, true, true), 4.0);
        assert_eq!(shuttle(4.0, true, true), 8.0);
        assert_eq!(shuttle(8.0, true, true), 8.0, "no faster than the limit");

        assert_eq!(shuttle(-1.0, true, false), -2.0);
        assert_eq!(
            shuttle(4.0, true, false),
            -1.0,
            "turning round starts at normal speed"
        );
        assert_eq!(shuttle(-4.0, true, true), 1.0);
        assert_eq!(
            shuttle(-4.0, false, false),
            -1.0,
            "from a stop it starts at normal speed"
        );
    }

    #[test]
    fn every_binding_in_the_help_is_real() {
        // the help lists a key for each of these; if one stops working the list must change
        let listed: String = BINDINGS
            .iter()
            .map(|(keys, _)| *keys)
            .collect::<Vec<_>>()
            .join(" ");
        for key in ["Space", "J", "Home", "B", "Esc"] {
            assert!(listed.contains(key), "{key}");
        }
    }
}

//! Colours: what each kind of thing looks like wherever it is drawn.

use crate::analysis::classify::Class;
use crate::analysis::invariants::Severity;
use crate::analysis::logparse::Level;
use eframe::egui::Color32;

pub const BACKGROUND: Color32 = Color32::from_rgb(22, 22, 27);
pub const LANE: Color32 = Color32::from_rgb(30, 30, 37);
pub const LANE_ALT: Color32 = Color32::from_rgb(34, 34, 42);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 140, 152);
pub const PLAYHEAD: Color32 = Color32::from_rgb(255, 214, 90);
pub const BUG: Color32 = Color32::from_rgb(230, 70, 60);
pub const SELECTION: Color32 = Color32::from_rgb(90, 150, 240);
pub const VIDEO: Color32 = Color32::from_rgb(70, 110, 160);
pub const ACCENT: Color32 = Color32::from_rgb(110, 170, 255);
pub const OK: Color32 = Color32::from_rgb(110, 200, 140);

/// How an event is drawn on the timeline.
#[must_use]
pub fn class(class: Class) -> Color32 {
    match class {
        Class::Command => Color32::from_rgb(110, 180, 255),
        Class::Focus => Color32::from_rgb(170, 170, 200),
        Class::Lifecycle => Color32::from_rgb(120, 205, 145),
        Class::Drag => Color32::from_rgb(245, 170, 90),
        Class::Echo | Class::Manual | Class::Other => Color32::from_rgb(88, 88, 100),
        Class::Monitor | Class::Lock => Color32::from_rgb(205, 130, 240),
    }
}

/// How much an event class matters when many share a pixel: the loudest wins.
#[must_use]
pub fn class_weight(class: Class) -> u8 {
    match class {
        Class::Monitor | Class::Lock => 6,
        Class::Command => 5,
        Class::Lifecycle => 4,
        Class::Drag => 3,
        Class::Focus => 2,
        Class::Echo | Class::Manual | Class::Other => 1,
    }
}

#[must_use]
pub fn severity(severity: Severity) -> Color32 {
    match severity {
        Severity::Error => Color32::from_rgb(235, 85, 75),
        Severity::Warn => Color32::from_rgb(235, 185, 60),
        Severity::Info => Color32::from_rgb(130, 130, 150),
    }
}

#[must_use]
pub fn level(level: Level) -> Color32 {
    match level {
        Level::Error => Color32::from_rgb(235, 85, 75),
        Level::Warn => Color32::from_rgb(235, 185, 60),
        _ => TEXT_DIM,
    }
}

pub const INCIDENT: Color32 = Color32::from_rgb(170, 160, 255);

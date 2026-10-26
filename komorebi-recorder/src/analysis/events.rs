//! komorebi's subscriber events, understood.
//!
//! `events.jsonl` keeps each notification's JSON as komorebi serialized it. Three kinds reach a
//! subscriber and they look different on the wire:
//!
//! - window-manager events, from the OS: `{"type":"MoveEnd","content":[{"type":"Accessibility",
//!   "content":"AXWindowMoved"},<pid>,<window id>]}`
//! - commands, from `komorebic` or a hotkey daemon: `{"type":"FocusWorkspaceNumber","content":0}`
//! - monitor changes: `{"DisplayConnectionChange":<display id>}`
//!
//! This module reads them into an [`EventRecord`] with the name, process and window pulled out.

use crate::session::Session;
use crate::sources::komorebi_events::EventRow;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    /// Something the operating system reported about a window or application.
    WindowManager,
    /// A command sent to komorebi.
    Command,
    /// A display was connected, disconnected or resized.
    Monitor,
    /// The notification could not be read.
    Unknown,
}

/// The names komorebi gives the events the OS reports.
const WINDOW_MANAGER_EVENTS: [&str; 14] = [
    "FocusChange",
    "Show",
    "Destroy",
    "Minimize",
    "Restore",
    "Manage",
    "Unmanage",
    "MoveStart",
    "MoveEnd",
    "ResizeStart",
    "ResizeEnd",
    "SpaceChange",
    "ScreenLock",
    "ScreenUnlock",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EventInfo {
    pub family: Family,
    /// `MoveEnd`, `FocusWorkspaceNumber`, `DisplayConnectionChange`, ...
    pub name: String,
    /// What the OS said, e.g. `AXWindowMoved`; window-manager events only.
    pub notification: Option<String>,
    pub pid: Option<i32>,
    pub window_id: Option<u32>,
    /// A command's argument or a monitor event's display id, as compact JSON.
    pub argument: Option<String>,
}

impl EventInfo {
    /// One line saying what this is: `MoveEnd pid 20934 AXWindowMoved`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut text = self.name.clone();
        if let Some(argument) = &self.argument {
            text.push(' ');
            text.push_str(argument);
        }
        if let Some(pid) = self.pid {
            text.push_str(&format!(" pid {pid}"));
        }
        if let Some(window_id) = self.window_id {
            text.push_str(&format!(" window {window_id}"));
        }
        if let Some(notification) = &self.notification {
            text.push(' ');
            text.push_str(notification);
        }
        text
    }
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 80 {
        let cut: String = text.chars().take(79).collect();
        format!("{cut}…")
    } else {
        text
    }
}

/// Read the JSON komorebi sent as `event`.
#[must_use]
pub fn describe(event: &Value) -> EventInfo {
    let unknown = |name: &str| EventInfo {
        family: Family::Unknown,
        name: name.to_string(),
        notification: None,
        pid: None,
        window_id: None,
        argument: None,
    };

    // a monitor notification is a one-entry object: {"Resize": 69733248}
    if event.get("type").is_none()
        && let Some(object) = event.as_object()
        && object.len() == 1
        && let Some((name, value)) = object.iter().next()
    {
        return EventInfo {
            family: Family::Monitor,
            name: name.clone(),
            notification: None,
            pid: None,
            window_id: None,
            argument: Some(compact(value)),
        };
    }

    // a unit-variant command is a bare string
    if let Some(name) = event.as_str() {
        return EventInfo {
            family: Family::Command,
            name: name.to_string(),
            ..unknown(name)
        };
    }

    let Some(name) = event.get("type").and_then(Value::as_str) else {
        return unknown("unreadable");
    };
    let content = event.get("content");

    if WINDOW_MANAGER_EVENTS.contains(&name) {
        let parts = content.and_then(Value::as_array);
        let at = |index: usize| parts.and_then(|parts| parts.get(index));
        return EventInfo {
            family: Family::WindowManager,
            name: name.to_string(),
            notification: at(0)
                .and_then(|notification| notification.get("content"))
                .and_then(Value::as_str)
                .map(str::to_string),
            pid: at(1)
                .and_then(Value::as_i64)
                .and_then(|pid| i32::try_from(pid).ok()),
            window_id: at(2)
                .and_then(Value::as_u64)
                .and_then(|id| u32::try_from(id).ok()),
            argument: None,
        };
    }

    EventInfo {
        family: Family::Command,
        name: name.to_string(),
        notification: None,
        pid: None,
        window_id: None,
        argument: content.filter(|value| !value.is_null()).map(compact),
    }
}

/// One recorded notification.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct EventRecord {
    pub seq: u64,
    pub recv_mach_ns: u64,
    pub recv_wall_us: i64,
    /// The state snapshot after this event (`states/<id>.json`); the same id as the previous event
    /// means the state did not change.
    pub state_id: Option<u64>,
    #[serde(flatten)]
    pub info: EventInfo,
    pub parse_error: Option<String>,
}

impl EventRecord {
    #[must_use]
    pub fn from_row(row: &EventRow) -> Self {
        let info = row.event.as_ref().map_or_else(
            || EventInfo {
                family: Family::Unknown,
                name: "unreadable".to_string(),
                notification: None,
                pid: None,
                window_id: None,
                argument: None,
            },
            describe,
        );

        Self {
            seq: row.seq,
            recv_mach_ns: row.recv_mach_ns,
            recv_wall_us: row.recv_wall_us,
            state_id: row.state_id,
            info,
            parse_error: row.parse_error.clone(),
        }
    }
}

/// Every event of a session, in the order they were received. Unreadable lines are skipped and
/// counted in the returned warnings.
pub fn load(session: &Session) -> eyre::Result<(Vec<EventRecord>, Vec<String>)> {
    let path = session.path("events.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(error) => {
            return Err(error).wrap_err_with(|| format!("could not read {}", path.display()));
        }
    };

    let mut records = Vec::new();
    let mut skipped = 0_usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<EventRow>(line) {
            Ok(row) => records.push(EventRecord::from_row(&row)),
            Err(_) => skipped += 1,
        }
    }

    records.sort_by_key(|record| (record.recv_mach_ns, record.seq));
    let warnings = if skipped > 0 {
        vec![format!(
            "{skipped} unreadable lines of events.jsonl were skipped"
        )]
    } else {
        Vec::new()
    };
    Ok((records, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_window_manager_event_has_its_process_window_and_notification() {
        let info = describe(&json!({
            "type": "MoveEnd",
            "content": [{"type": "Accessibility", "content": "AXWindowMoved"}, 20934, 4711]
        }));

        assert_eq!(info.family, Family::WindowManager);
        assert_eq!(info.name, "MoveEnd");
        assert_eq!(info.notification.as_deref(), Some("AXWindowMoved"));
        assert_eq!(info.pid, Some(20934));
        assert_eq!(info.window_id, Some(4711));
        assert_eq!(
            info.summary(),
            "MoveEnd pid 20934 window 4711 AXWindowMoved"
        );
    }

    #[test]
    fn a_focus_change_may_have_no_window() {
        let info = describe(&json!({
            "type": "FocusChange",
            "content": [{"type": "AppKitWorkspace", "content": "NSWorkspaceDidActivateApplicationNotification"}, 79469, null]
        }));

        assert_eq!(info.pid, Some(79469));
        assert_eq!(info.window_id, None);
        assert_eq!(
            info.notification.as_deref(),
            Some("NSWorkspaceDidActivateApplicationNotification")
        );
    }

    #[test]
    fn commands_carry_their_argument() {
        let info = describe(&json!({"type": "FocusWorkspaceNumber", "content": 2}));
        assert_eq!(info.family, Family::Command);
        assert_eq!(info.name, "FocusWorkspaceNumber");
        assert_eq!(info.argument.as_deref(), Some("2"));

        let bare = describe(&json!({"type": "Retile"}));
        assert_eq!(bare.family, Family::Command);
        assert_eq!(bare.argument, None);

        assert_eq!(describe(&json!("Retile")).family, Family::Command);
    }

    #[test]
    fn monitor_notifications_are_one_entry_objects() {
        let info = describe(&json!({"DisplayConnectionChange": 1}));
        assert_eq!(info.family, Family::Monitor);
        assert_eq!(info.name, "DisplayConnectionChange");
        assert_eq!(info.argument.as_deref(), Some("1"));
    }

    #[test]
    fn nonsense_is_unknown_not_a_panic() {
        assert_eq!(describe(&json!(12)).family, Family::Unknown);
        assert_eq!(describe(&json!({"a": 1, "b": 2})).family, Family::Unknown);
        assert_eq!(describe(&json!({"type": 3})).family, Family::Unknown);
    }

    #[test]
    fn loading_sorts_by_receipt_and_skips_what_it_cannot_read() {
        let dir =
            std::env::temp_dir().join(format!("komorebi-recorder-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let session = Session::create(Some(dir.clone()), chrono::Utc::now()).expect("create");
        std::fs::write(
            session.path("events.jsonl"),
            concat!(
                "{\"seq\":2,\"recv_wall_us\":0,\"recv_mach_ns\":20,\"event\":{\"type\":\"Retile\"}}\n",
                "garbage\n",
                "{\"seq\":1,\"recv_wall_us\":0,\"recv_mach_ns\":10,\"state_id\":4}\n",
            ),
        )
        .expect("events");

        let (records, warnings) = load(&session).expect("load");
        assert_eq!(
            records.iter().map(|record| record.seq).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(records[0].state_id, Some(4));
        assert_eq!(records[0].info.family, Family::Unknown);
        assert_eq!(records[1].info.name, "Retile");
        assert_eq!(warnings.len(), 1);
    }
}

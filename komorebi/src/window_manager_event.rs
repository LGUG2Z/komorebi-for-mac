#![allow(non_upper_case_globals)]

use crate::accessibility::notification_constants::AccessibilityNotification;
use serde::Deserialize;
use serde::Serialize;
use strum::Display;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Display)]
#[serde(tag = "type", content = "content")]
pub enum WindowManagerEvent {
    FocusChange(AccessibilityNotification, i32, Option<u32>),
    Show(AccessibilityNotification, i32),
    Destroy(AccessibilityNotification, i32),
    Minimize(AccessibilityNotification, i32, u32),
    Restore(AccessibilityNotification, i32, u32),
}

impl WindowManagerEvent {
    pub fn from_ax_notification(
        notification: AccessibilityNotification,
        process_id: i32,
        window_id: Option<u32>,
    ) -> Option<Self> {
        match notification {
            AccessibilityNotification::AXMainWindowChanged
            | AccessibilityNotification::AXApplicationActivated => Some(
                WindowManagerEvent::FocusChange(notification, process_id, window_id),
            ),
            AccessibilityNotification::AXWindowCreated
            | AccessibilityNotification::AXApplicationShown => {
                Some(WindowManagerEvent::Show(notification, process_id))
            }
            AccessibilityNotification::AXUIElementDestroyed => {
                Some(WindowManagerEvent::Destroy(notification, process_id))
            }
            AccessibilityNotification::AXWindowMiniaturized => window_id
                .map(|window_id| WindowManagerEvent::Minimize(notification, process_id, window_id)),
            AccessibilityNotification::AXWindowDeminiaturized => window_id
                .map(|window_id| WindowManagerEvent::Restore(notification, process_id, window_id)),
            // kAXWindowMovedNotification => {}
            // kAXWindowResizedNotification => {}
            // kAXTitleChangedNotification => {}
            // kAXApplicationDeactivatedNotification => {}
            _ => None,
        }
    }
}

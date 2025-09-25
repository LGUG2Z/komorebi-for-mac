#![allow(non_upper_case_globals)]

use crate::accessibility::notification_constants::AccessibilityNotification;
use crate::app_kit_notification_constants::AppKitWorkspaceNotification;
use serde::Deserialize;
use serde::Serialize;
use strum::Display;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Display)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "content")]
pub enum SystemNotification {
    Accessibility(AccessibilityNotification),
    AppKitWorkspace(AppKitWorkspaceNotification),
    Manual(ManualNotification),
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Display)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum ManualNotification {
    ShowOnInputEvent,
    ShowOnFocusChangeFirstTabDestroyed,
    ShowOnFocusChangeWindowlessAppRestored,
    Manage,
    Unmanage,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Display)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "content")]
pub enum WindowManagerEvent {
    FocusChange(SystemNotification, i32, Option<u32>),
    Show(SystemNotification, i32),
    Destroy(SystemNotification, i32),
    Minimize(SystemNotification, i32, u32),
    Restore(SystemNotification, i32, u32),
    Manage(SystemNotification, i32, u32),
    Unmanage(SystemNotification, i32, u32),
}

impl WindowManagerEvent {
    pub fn from_system_notification(
        notification: SystemNotification,
        process_id: i32,
        window_id: Option<u32>,
    ) -> Option<Self> {
        match notification {
            SystemNotification::Accessibility(AccessibilityNotification::AXMainWindowChanged)
            | SystemNotification::Accessibility(
                AccessibilityNotification::AXApplicationActivated,
            )
            | SystemNotification::AppKitWorkspace(
                AppKitWorkspaceNotification::NSWorkspaceDidActivateApplicationNotification,
            ) => Some(WindowManagerEvent::FocusChange(
                notification,
                process_id,
                window_id,
            )),
            SystemNotification::Accessibility(AccessibilityNotification::AXWindowCreated)
            | SystemNotification::Accessibility(AccessibilityNotification::AXApplicationShown)
            | SystemNotification::AppKitWorkspace(
                AppKitWorkspaceNotification::NSWorkspaceDidLaunchApplicationNotification,
            )
            | SystemNotification::Manual(
                ManualNotification::ShowOnFocusChangeWindowlessAppRestored,
            )
            | SystemNotification::Manual(ManualNotification::ShowOnFocusChangeFirstTabDestroyed)
            | SystemNotification::Manual(ManualNotification::ShowOnInputEvent) => {
                Some(WindowManagerEvent::Show(notification, process_id))
            }
            SystemNotification::Accessibility(AccessibilityNotification::AXUIElementDestroyed)
            | SystemNotification::AppKitWorkspace(
                AppKitWorkspaceNotification::NSWorkspaceDidTerminateApplicationNotification,
            ) => Some(WindowManagerEvent::Destroy(notification, process_id)),
            // TODO: figure out if we wanna handle the hide/unhide notifications separately
            // TODO: maybe turn window id into a vec of window IDs for hidden apps
            SystemNotification::Accessibility(AccessibilityNotification::AXWindowMiniaturized) => {
                window_id.map(|window_id| {
                    WindowManagerEvent::Minimize(notification, process_id, window_id)
                })
            }
            SystemNotification::Accessibility(
                AccessibilityNotification::AXWindowDeminiaturized,
            ) => window_id
                .map(|window_id| WindowManagerEvent::Restore(notification, process_id, window_id)),
            SystemNotification::Manual(ManualNotification::Manage) => window_id
                .map(|window_id| WindowManagerEvent::Manage(notification, process_id, window_id)),
            SystemNotification::Manual(ManualNotification::Unmanage) => window_id
                .map(|window_id| WindowManagerEvent::Unmanage(notification, process_id, window_id)),
            // kAXWindowMovedNotification => {}
            // kAXWindowResizedNotification => {}
            // kAXTitleChangedNotification => {}
            // kAXApplicationDeactivatedNotification => {}
            _ => None,
        }
    }

    pub fn process_id(&self) -> i32 {
        match self {
            WindowManagerEvent::FocusChange(_, process_id, _)
            | WindowManagerEvent::Show(_, process_id)
            | WindowManagerEvent::Destroy(_, process_id)
            | WindowManagerEvent::Minimize(_, process_id, _)
            | WindowManagerEvent::Manage(_, process_id, _)
            | WindowManagerEvent::Unmanage(_, process_id, _)
            | WindowManagerEvent::Restore(_, process_id, _) => *process_id,
        }
    }

    pub fn notification(&self) -> String {
        match self {
            WindowManagerEvent::FocusChange(n, _, _)
            | WindowManagerEvent::Show(n, _)
            | WindowManagerEvent::Destroy(n, _)
            | WindowManagerEvent::Minimize(n, _, _)
            | WindowManagerEvent::Manage(n, _, _)
            | WindowManagerEvent::Unmanage(n, _, _)
            | WindowManagerEvent::Restore(n, _, _) => match n {
                SystemNotification::Accessibility(a) => a.to_string(),
                SystemNotification::AppKitWorkspace(a) => a.to_string(),
                SystemNotification::Manual(m) => m.to_string(),
            },
        }
    }

    pub fn window_id(&self) -> Option<u32> {
        match self {
            WindowManagerEvent::FocusChange(_, _, window_id) => *window_id,
            WindowManagerEvent::Show(_, _) | WindowManagerEvent::Destroy(_, _) => None,
            WindowManagerEvent::Minimize(_, _, window_id)
            | WindowManagerEvent::Manage(_, _, window_id)
            | WindowManagerEvent::Unmanage(_, _, window_id)
            | WindowManagerEvent::Restore(_, _, window_id) => Some(*window_id),
        }
    }
}

use crate::AccessibilityObserver;
use crate::AccessibilityUiElement;
use crate::LibraryError;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::attribute_constants::kAXMainAttribute;
use crate::accessibility::attribute_constants::kAXMinimizedAttribute;
use crate::accessibility::attribute_constants::kAXPositionAttribute;
use crate::accessibility::attribute_constants::kAXSizeAttribute;
use crate::accessibility::attribute_constants::kAXTitleAttribute;
use crate::accessibility::error::AccessibilityCustomError;
use crate::accessibility::error::AccessibilityError;
use crate::accessibility::notification_constants::AccessibilityNotification;
use crate::accessibility::notification_constants::kAXTitleChangedNotification;
use crate::accessibility::notification_constants::kAXWindowDeminiaturizedNotification;
use crate::accessibility::notification_constants::kAXWindowMiniaturizedNotification;
use crate::accessibility::notification_constants::kAXWindowMovedNotification;
use crate::accessibility::notification_constants::kAXWindowResizedNotification;
use crate::application::Application;
use crate::ax_event_listener::event_tx;
use crate::cf_dictionary_value;
use crate::core::rect::Rect;
use crate::core_graphics::CoreGraphicsApi;
use crate::hidden_frame_bottom_left;
use crate::macos_api::MacosApi;
use crate::window_manager_event::WindowManagerEvent;
use objc2_app_kit::NSApplicationActivationOptions;
use objc2_app_kit::NSRunningApplication;
use objc2_application_services::AXObserver;
use objc2_application_services::AXUIElement;
use objc2_application_services::AXValueType;
use objc2_core_foundation::CFBoolean;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFNumber;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use objc2_core_foundation::CFString;
use objc2_core_foundation::CGFloat;
use objc2_core_foundation::CGPoint;
use objc2_core_foundation::CGSize;
use objc2_core_graphics::kCGWindowAlpha;
use objc2_core_graphics::kCGWindowBounds;
use objc2_core_graphics::kCGWindowName;
use objc2_core_graphics::kCGWindowOwnerName;
use objc2_core_graphics::kCGWindowOwnerPID;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::str::FromStr;
use tracing::instrument;

const NOTIFICATIONS: &[&str] = &[
    kAXWindowMiniaturizedNotification,
    kAXWindowDeminiaturizedNotification,
    kAXWindowMovedNotification,
    kAXWindowResizedNotification,
    kAXTitleChangedNotification,
];

#[instrument(skip_all)]
unsafe extern "C-unwind" fn window_observer_callback(
    _observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    _context: *mut c_void,
) {
    unsafe {
        let name =
            AccessibilityApi::copy_attribute_value::<CFString>(element.as_ref(), kAXTitleAttribute)
                .map(|s| s.to_string());

        if let Some(name) = name
            && !name.is_empty()
        {
            let mut process_id = 0;
            element.as_ref().pid(NonNull::from_mut(&mut process_id));

            let window_id = AccessibilityApi::window_id(element.as_ref()).ok();

            if let Ok(notification) =
                AccessibilityNotification::from_str(&notification.as_ref().to_string())
                && let Some(event) =
                    WindowManagerEvent::from_ax_notification(notification, process_id, window_id)
            {
                if let Err(error) = event_tx().send(event) {
                    tracing::error!("failed to send window manager event: {error}");
                } else {
                    tracing::debug!(
                        "notification: {}, process: {process_id}, name: \"{name}\"",
                        notification,
                    );
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct WindowInfo {
    name: Option<String>,
    owner_pid: i32,
    owner_name: String,
    alpha: f32,
    bounds: WindowBounds,
}

impl WindowInfo {
    pub fn new(entry: NonNull<CFDictionary>) -> Self {
        WindowInfo::from(unsafe { entry.as_ref() })
    }
}

#[derive(Debug, Default)]
#[allow(unused)]
pub struct ValidWindowInfo {
    pub name: String,
    pub owner_pid: i32,
    owner_name: String,
    alpha: f32,
    pub bounds: WindowBounds,
}

impl WindowInfo {
    pub fn validated(self) -> Option<ValidWindowInfo> {
        if let Some(name) = self.name
            && self.alpha != 0.0
            && self.bounds.y != 0.0
            && self.bounds.height != 0.0
            && !name.is_empty()
        {
            return Some(ValidWindowInfo {
                name,
                owner_pid: self.owner_pid,
                owner_name: self.owner_name,
                alpha: self.alpha,
                bounds: self.bounds,
            });
        }

        None
    }
}

#[derive(Debug, Clone)]
#[allow(unused)]
pub struct Window {
    pub id: u32,
    element: AccessibilityUiElement,
    pub application: Application,
    observer: AccessibilityObserver,
    pub restore_point: Option<(f32, f32)>,
}

impl Drop for Window {
    fn drop(&mut self) {
        // this gets called when a cloned Window is dropped, so we need to make sure it only
        // invalidates the observer if the Window is no longer open
        if !self.is_valid() {
            tracing::info!(
                "invalidating window observer for {}",
                self.title()
                    .unwrap_or_else(|| String::from("<NO TITLE FOUND>"))
            );

            // make sure the observer gets removed from any run loops
            AccessibilityApi::invalidate_observer(&self.observer);
        }
    }
}

impl Window {
    pub fn new(
        element: CFRetained<AXUIElement>,
        application: Application,
    ) -> Result<Self, AccessibilityError> {
        let observer = AccessibilityApi::create_observer(
            application.process_id,
            Some(window_observer_callback),
        )?;

        Ok(Self {
            id: AccessibilityApi::window_id(&element)?,
            element: AccessibilityUiElement(element),
            application,
            observer: AccessibilityObserver(observer),
            restore_point: None,
        })
    }

    pub fn is_valid(&self) -> bool {
        AccessibilityApi::copy_attribute_names(&self.element).is_some()
    }

    #[tracing::instrument(skip_all)]
    pub fn observe(&self, run_loop: &CFRunLoop) -> Result<(), AccessibilityError> {
        tracing::info!(
            "registering observer for process: {}, title: {}",
            self.application.process_id,
            self.title()
                .unwrap_or_else(|| String::from("<NO TITLE FOUND>"))
        );

        AccessibilityApi::add_observer_to_run_loop(
            &self.observer,
            &self.element,
            NOTIFICATIONS,
            run_loop,
        )
    }

    #[tracing::instrument(skip_all)]
    pub fn hide(&mut self) -> Result<(), AccessibilityError> {
        // I don't love this, but it's basically what Aerospace does in lieu of an actual "Hide" API
        if self.restore_point.is_none() {
            let rect = MacosApi::window_rect(&self.element)?;
            if let Some(monitor_size) = CoreGraphicsApi::display_bounds_for_window_rect(rect) {
                self.restore_point = Some((rect.origin.x as f32, rect.origin.y as f32));
                let hidden_rect = hidden_frame_bottom_left(monitor_size, rect.size);

                tracing::debug!(
                    "hiding {} and setting restore point to {},{}",
                    self.title()
                        .unwrap_or_else(|| String::from("<NO TITLE FOUND>")),
                    rect.origin.x,
                    rect.origin.y,
                );

                self.set_point(hidden_rect.origin)?;
                self.set_size(hidden_rect.size)?;
            }
        }

        Ok(())
    }

    pub fn minimize(&mut self) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(true);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXMinimizedAttribute, value)
    }

    pub fn unminimize(&mut self) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(false);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXMinimizedAttribute, value)
    }

    #[tracing::instrument(skip_all)]
    pub fn restore(&mut self) -> Result<(), AccessibilityError> {
        let mut should_unset_restore_point = false;
        if let Some((x, y)) = self.restore_point {
            tracing::debug!(
                "restoring {:?} to point {x},{y}",
                self.title()
                    .unwrap_or_else(|| String::from("<NO TITLE FOUND>"))
            );

            self.set_point(CGPoint::new(x as CGFloat, y as CGFloat))?;
            should_unset_restore_point = true;
        }

        if should_unset_restore_point {
            self.restore_point = None;
        }

        Ok(())
    }

    pub fn title(&self) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(&self.element, kAXTitleAttribute)
            .map(|s| s.to_string())
    }

    pub fn set_position(&self, rect: &Rect) -> Result<(), AccessibilityError> {
        self.set_point(CGPoint::new(rect.left as CGFloat, rect.top as CGFloat))?;
        self.set_size(CGSize::new(rect.right as CGFloat, rect.bottom as CGFloat))
    }

    pub fn focus(&self, focus_follows_mouse: bool) -> Result<(), LibraryError> {
        unsafe {
            NSRunningApplication::runningApplicationWithProcessIdentifier(
                self.application.process_id,
            )
            .ok_or(AccessibilityError::Custom(
                AccessibilityCustomError::NSRunningApplication(self.application.process_id),
            ))?
            .activateWithOptions(NSApplicationActivationOptions::empty());

            let cf_boolean = CFBoolean::new(true);
            let value = &**cf_boolean;
            AccessibilityApi::set_attribute_cf_value(&self.element, kAXMainAttribute, value)?;
        }

        if focus_follows_mouse {
            // MacosApi::center_cursor_in_rect(&MacosApi::window_rect(&self.element))?
        }

        Ok(())
    }

    pub fn set_point(&self, point: CGPoint) -> Result<(), AccessibilityError> {
        AccessibilityApi::set_attribute_ax_value(
            &self.element,
            kAXPositionAttribute,
            AXValueType::CGPoint,
            point,
        )
    }

    pub fn set_size(&self, size: CGSize) -> Result<(), AccessibilityError> {
        AccessibilityApi::set_attribute_ax_value(
            &self.element,
            kAXSizeAttribute,
            AXValueType::CGSize,
            size,
        )
    }
}

impl From<&CFDictionary> for WindowInfo {
    fn from(value: &CFDictionary) -> Self {
        unsafe {
            Self {
                name: cf_dictionary_value::<CFString>(value, kCGWindowName)
                    .map(|s| s.as_ref().to_string())
                    .and_then(|s| if s.is_empty() { None } else { Some(s) }),
                owner_pid: cf_dictionary_value::<CFNumber>(value, kCGWindowOwnerPID)
                    .and_then(|s| s.as_ref().as_i32())
                    .expect("window must have an owner process id"),
                owner_name: cf_dictionary_value::<CFString>(value, kCGWindowOwnerName)
                    .map(|s| s.as_ref().to_string())
                    .expect("window must have an owner name"),
                alpha: cf_dictionary_value::<CFNumber>(value, kCGWindowAlpha)
                    .and_then(|s| s.as_ref().as_f32())
                    .expect("window must have an alpha value"),

                bounds: if let Some(dict) =
                    cf_dictionary_value::<CFDictionary>(value, kCGWindowBounds).as_ref()
                {
                    WindowBounds::from(dict.as_ref())
                } else {
                    Default::default()
                },
            }
        }
    }
}

#[derive(Default, Debug, Copy, Clone)]
#[allow(unused)]
pub struct WindowBounds {
    pub height: f32,
    pub width: f32,
    pub x: f32,
    pub y: f32,
}

impl From<&CFDictionary> for WindowBounds {
    fn from(value: &CFDictionary) -> Self {
        unsafe {
            Self {
                height: cf_dictionary_value::<CFNumber>(
                    value,
                    &CFString::from_static_str("Height"),
                )
                .and_then(|val| val.as_ref().as_f32())
                .unwrap_or_default(),
                width: cf_dictionary_value::<CFNumber>(value, &CFString::from_static_str("Width"))
                    .and_then(|val| val.as_ref().as_f32())
                    .unwrap_or_default(),
                x: cf_dictionary_value::<CFNumber>(value, &CFString::from_static_str("X"))
                    .and_then(|val| val.as_ref().as_f32())
                    .unwrap_or_default(),
                y: cf_dictionary_value::<CFNumber>(value, &CFString::from_static_str("Y"))
                    .and_then(|val| val.as_ref().as_f32())
                    .unwrap_or_default(),
            }
        }
    }
}

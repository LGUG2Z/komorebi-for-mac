use crate::AccessibilityObserver;
use crate::AccessibilityUiElement;
use crate::FLOATING_WINDOW_TOGGLE_ASPECT_RATIO;
use crate::LibraryError;
use crate::WINDOW_RESTORE_POSITIONS;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::attribute_constants::kAXFocusedAttribute;
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
use crate::window_manager_event::SystemNotification;
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
use serde::Deserialize;
use serde::Serialize;
use std::collections::hash_map::Entry;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::str::FromStr;
use strum::Display;
use strum::EnumString;
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
                && let Some(event) = WindowManagerEvent::from_system_notification(
                    SystemNotification::Accessibility(notification),
                    process_id,
                    window_id,
                )
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
pub struct Window {
    pub id: u32,
    pub element: AccessibilityUiElement,
    pub application: Application,
    observer: AccessibilityObserver,
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
        let mut window_restore_positions = WINDOW_RESTORE_POSITIONS.lock();
        if let Entry::Vacant(entry) = window_restore_positions.entry(self.id) {
            let rect = MacosApi::window_rect(&self.element)?;
            if let Some(monitor_size) = CoreGraphicsApi::display_bounds_for_window_rect(rect) {
                entry.insert(rect);
                drop(window_restore_positions);

                // I don't love this, but it's basically what Aerospace does in lieu of an actual "Hide" API
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
        let mut should_remove_restore_position = false;
        let mut window_restore_positions = WINDOW_RESTORE_POSITIONS.lock();
        if let Some(cg_rect) = window_restore_positions.get(&self.id) {
            tracing::debug!(
                "restoring {:?} to {cg_rect:?}",
                self.title()
                    .unwrap_or_else(|| String::from("<NO TITLE FOUND>"))
            );

            self.set_point(cg_rect.origin)?;
            self.set_size(cg_rect.size)?;
            should_remove_restore_position = true;
        }

        if should_remove_restore_position {
            window_restore_positions.remove(&self.id);
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

    pub fn focus(&self, mouse_follows_focus: bool) -> Result<(), LibraryError> {
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

        if mouse_follows_focus {
            MacosApi::center_cursor_in_rect(&MacosApi::window_rect(&self.element)?.into())?
        }

        Ok(())
    }

    pub fn raise(&self) -> Result<(), AccessibilityError> {
        let cf_boolean = CFBoolean::new(true);
        let value = &**cf_boolean;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXMainAttribute, value)?;
        AccessibilityApi::set_attribute_cf_value(&self.element, kAXFocusedAttribute, value)
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

    pub fn center(&mut self, work_area: &Rect, resize: bool) -> Result<(), AccessibilityError> {
        let (target_width, target_height) = if resize {
            let (aspect_ratio_width, aspect_ratio_height) = FLOATING_WINDOW_TOGGLE_ASPECT_RATIO
                .lock()
                .width_and_height();
            let target_height = work_area.bottom / 2;
            let target_width = (target_height * aspect_ratio_width) / aspect_ratio_height;
            (target_width, target_height)
        } else {
            let current_rect = Rect::from(MacosApi::window_rect(&self.element)?);
            (current_rect.right, current_rect.bottom)
        };

        let x = work_area.left + ((work_area.right - target_width) / 2);
        let y = work_area.top + ((work_area.bottom - target_height) / 2);

        self.set_position(&Rect {
            left: x,
            top: y,
            right: target_width,
            bottom: target_height,
        })
    }

    pub fn move_to_area(
        &self,
        current_area: &Rect,
        target_area: &Rect,
    ) -> Result<(), AccessibilityError> {
        let current_rect = Rect::from(MacosApi::window_rect(&self.element)?);
        let x_diff = target_area.left - current_area.left;
        let y_diff = target_area.top - current_area.top;
        let x_ratio = f32::abs((target_area.right as f32) / (current_area.right as f32));
        let y_ratio = f32::abs((target_area.bottom as f32) / (current_area.bottom as f32));
        let window_relative_x = current_rect.left - current_area.left;
        let window_relative_y = current_rect.top - current_area.top;
        let corrected_relative_x = (window_relative_x as f32 * x_ratio) as i32;
        let corrected_relative_y = (window_relative_y as f32 * y_ratio) as i32;
        let window_x = current_area.left + corrected_relative_x;
        let window_y = current_area.top + corrected_relative_y;
        let left = x_diff + window_x;
        let top = y_diff + window_y;

        let corrected_width = (current_rect.right as f32 * x_ratio) as i32;
        let corrected_height = (current_rect.bottom as f32 * y_ratio) as i32;

        let new_rect = Rect {
            left,
            top,
            right: corrected_width,
            bottom: corrected_height,
        };

        // TODO: figure out what to do about maximized windows on macOS
        // let is_maximized = &new_rect == target_area;
        // if is_maximized {
        //     windows_api::WindowsApi::unmaximize_window(self.hwnd);
        //     let animation_enabled = ANIMATION_ENABLED_PER_ANIMATION.lock();
        //     let move_enabled = animation_enabled
        //         .get(&MovementRenderDispatcher::PREFIX)
        //         .is_some_and(|v| *v);
        //     drop(animation_enabled);
        //
        //     if move_enabled || ANIMATION_ENABLED_GLOBAL.load(Ordering::SeqCst) {
        //         let anim_count = ANIMATION_MANAGER
        //             .lock()
        //             .count_in_progress(MovementRenderDispatcher::PREFIX);
        //         self.set_position(&new_rect, true)?;
        //         let hwnd = self.hwnd;
        //         // Wait for the animation to finish before maximizing the window again, otherwise
        //         // we would be maximizing the window on the current monitor anyway
        //         thread::spawn(move || {
        //             let mut new_anim_count = ANIMATION_MANAGER
        //                 .lock()
        //                 .count_in_progress(MovementRenderDispatcher::PREFIX);
        //             let mut max_wait = 2000; // Max waiting time. No one will be using an animation longer than 2s, right? RIGHT??? WHY?
        //             while new_anim_count > anim_count && max_wait > 0 {
        //                 thread::sleep(Duration::from_millis(10));
        //                 new_anim_count = ANIMATION_MANAGER
        //                     .lock()
        //                     .count_in_progress(MovementRenderDispatcher::PREFIX);
        //                 max_wait -= 1;
        //             }
        //             windows_api::WindowsApi::maximize_window(hwnd);
        //         });
        //     } else {
        //         self.set_position(&new_rect, true)?;
        //         windows_api::WindowsApi::maximize_window(self.hwnd);
        //     }
        // } else {
        self.set_position(&new_rect)?;
        // }

        Ok(())
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

#[derive(Copy, Clone, Debug, Display, EnumString, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AspectRatio {
    /// A predefined aspect ratio
    Predefined(PredefinedAspectRatio),
    /// A custom W:H aspect ratio
    Custom(i32, i32),
}

impl Default for AspectRatio {
    fn default() -> Self {
        AspectRatio::Predefined(PredefinedAspectRatio::default())
    }
}

#[derive(Copy, Clone, Debug, Default, Display, EnumString, Serialize, Deserialize, PartialEq)]
pub enum PredefinedAspectRatio {
    /// 21:9
    Ultrawide,
    /// 16:9
    Widescreen,
    /// 4:3
    #[default]
    Standard,
}

impl AspectRatio {
    pub fn width_and_height(self) -> (i32, i32) {
        match self {
            AspectRatio::Predefined(predefined) => match predefined {
                PredefinedAspectRatio::Ultrawide => (21, 9),
                PredefinedAspectRatio::Widescreen => (16, 9),
                PredefinedAspectRatio::Standard => (4, 3),
            },
            AspectRatio::Custom(w, h) => (w, h),
        }
    }
}

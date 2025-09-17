use crate::accessibility::AccessibilityApi;
use crate::accessibility::attribute_constants::kAXMainAttribute;
use crate::accessibility::attribute_constants::kAXPositionAttribute;
use crate::accessibility::attribute_constants::kAXSizeAttribute;
use crate::accessibility::attribute_constants::kAXTitleAttribute;
use crate::accessibility::error::AccessibilityCustomError;
use crate::accessibility::error::AccessibilityError;
use crate::accessibility::notification_constants::kAXTitleChangedNotification;
use crate::accessibility::notification_constants::kAXWindowDeminiaturizedNotification;
use crate::accessibility::notification_constants::kAXWindowMiniaturizedNotification;
use crate::accessibility::notification_constants::kAXWindowMovedNotification;
use crate::accessibility::notification_constants::kAXWindowResizedNotification;
use crate::application::Application;
use crate::cf_dictionary_value;
use crate::rect::Rect;
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
        let title =
            AccessibilityApi::copy_attribute_value::<CFString>(element.as_ref(), kAXTitleAttribute)
                .map(|s| s.to_string());

        let mut pid = 0;

        element.as_ref().pid(NonNull::from_mut(&mut pid));

        if let Some(title) = title
            && !title.is_empty()
        {
            tracing::info!(
                "notification: {}, process: {pid}, title: \"{title}\"",
                notification.as_ref()
            );
        } else {
            tracing::info!("notification: {}, process: {pid}", notification.as_ref());
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

#[derive(Debug)]
#[allow(unused)]
pub struct Window {
    element: CFRetained<AXUIElement>,
    application: Application,
    observer: CFRetained<AXObserver>,
}

impl Drop for Window {
    fn drop(&mut self) {
        // make sure the observer gets removed from any run loops
        tracing::info!(
            "invalidating window observer for {}",
            self.title()
                .unwrap_or_else(|| String::from("<NO TITLE FOUND>"))
        );
        AccessibilityApi::invalidate_observer(&self.observer);
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
            element,
            application,
            observer,
        })
    }

    #[tracing::instrument(skip_all)]
    pub fn observe(&self, run_loop: &CFRetained<CFRunLoop>) -> Result<(), AccessibilityError> {
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

    pub fn title(&self) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(&self.element, kAXTitleAttribute)
            .map(|s| s.to_string())
    }

    pub fn set_position(&self, rect: &Rect) -> Result<(), AccessibilityError> {
        self.set_point(CGPoint::new(rect.left as CGFloat, rect.top as CGFloat))?;
        self.set_size(CGSize::new(rect.right as CGFloat, rect.bottom as CGFloat))
    }

    pub fn focus(&self) -> Result<(), AccessibilityError> {
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
            AccessibilityApi::set_attribute_cf_value(&self.element, kAXMainAttribute, value)
        }
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

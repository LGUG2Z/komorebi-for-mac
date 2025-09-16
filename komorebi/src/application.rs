use crate::accessibility::AccessibilityApi;
use crate::accessibility::attribute_constants::kAXTitleAttribute;
use crate::accessibility::attribute_constants::kAXWindowsAttribute;
use crate::accessibility::error::AccessibilityError;
use crate::accessibility::notification_constants::kAXApplicationActivatedNotification;
use crate::accessibility::notification_constants::kAXApplicationDeactivatedNotification;
use crate::accessibility::notification_constants::kAXApplicationHiddenNotification;
use crate::accessibility::notification_constants::kAXApplicationShownNotification;
use crate::accessibility::notification_constants::kAXMainWindowChangedNotification;
use crate::accessibility::notification_constants::kAXUIElementDestroyedNotification;
use crate::accessibility::notification_constants::kAXWindowCreatedNotification;
use crate::window::Window;
use objc2_application_services::AXObserver;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFArray;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use objc2_core_foundation::CFString;
use std::ffi::c_void;
use std::ptr::NonNull;
use tracing::instrument;

const NOTIFICATIONS: &[&str] = &[
    kAXApplicationActivatedNotification,
    kAXApplicationDeactivatedNotification,
    kAXApplicationHiddenNotification,
    kAXApplicationShownNotification,
    // this is when we change focus between two windows of the same app
    kAXMainWindowChangedNotification,
    // this is when the same app has a new window opened
    kAXWindowCreatedNotification,
    // this is when a window of an application is destroyed / closed
    // when this fires, the app owner name won't be found, but the can be matched via PID
    kAXUIElementDestroyedNotification,
];

#[derive(Debug, Clone)]
pub struct Application {
    element: CFRetained<AXUIElement>,
    pub process_id: i32,
    pub observer: CFRetained<AXObserver>,
}

#[instrument(skip_all)]
unsafe extern "C-unwind" fn application_observer_callback(
    _observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    _context: *mut c_void,
) {
    unsafe {
        let name =
            AccessibilityApi::copy_attribute_value::<CFString>(element.as_ref(), kAXTitleAttribute)
                .map(|s| s.to_string());

        let mut pid = 0;

        element.as_ref().pid(NonNull::from_mut(&mut pid));

        if let Some(name) = name
            && !name.is_empty()
        {
            tracing::info!(
                "notification: {}, process: {pid}, name: \"{name}\"",
                notification.as_ref()
            );
        } else {
            tracing::info!("notification: {}, process: {pid}", notification.as_ref());
        }
    }
}

impl Drop for Application {
    fn drop(&mut self) {
        tracing::info!(
            "invalidating application observer for process id {}",
            self.process_id
        );
        // make sure the observer gets removed from any run loops
        AccessibilityApi::invalidate_observer(&self.observer);
    }
}

impl Application {
    pub fn new(process_id: i32) -> Result<Self, AccessibilityError> {
        Ok(Self {
            element: AccessibilityApi::create_application(process_id),
            process_id,
            observer: AccessibilityApi::create_observer(
                process_id,
                Some(application_observer_callback),
            )?,
        })
    }

    pub fn name(&self) -> Option<String> {
        AccessibilityApi::copy_attribute_value::<CFString>(&self.element, kAXTitleAttribute)
            .map(|s| s.to_string())
    }

    #[tracing::instrument(skip_all)]
    pub fn observe(&self, run_loop: &CFRetained<CFRunLoop>) -> Result<(), AccessibilityError> {
        tracing::info!(
            "registering observer for process: {}, name: {}",
            self.process_id,
            self.name()
                .unwrap_or_else(|| String::from("<NO NAME FOUND>"))
        );

        AccessibilityApi::add_observer_to_run_loop(
            &self.observer,
            &self.element,
            NOTIFICATIONS,
            run_loop,
        )
    }

    fn window_elements(&self) -> Option<CFRetained<CFArray<AXUIElement>>> {
        AccessibilityApi::copy_attribute_value::<CFArray<AXUIElement>>(
            &self.element,
            kAXWindowsAttribute,
        )
    }

    pub fn window_by_title(&self, title: &str) -> Option<Window> {
        let mut target = None;

        for element in self.window_elements()? {
            let window = Window::new(element, self.clone()).ok()?;

            if let Some(window_title) = window.title()
                && window_title.eq(title)
            {
                target = Some(window);
            }
        }

        target
    }
}

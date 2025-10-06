use crate::accessibility::AccessibilityApi;
use crate::macos_api::MacosApi;
use crate::reaper;
use crate::reaper::ReaperNotification;
use crate::window_manager_event::ManualNotification;
use crate::window_manager_event::SystemNotification;
use crate::window_manager_event::WindowManagerEvent;
use crate::window_manager_event_listener;
use objc2_core_foundation::CFMachPort;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use objc2_core_foundation::kCFRunLoopDefaultMode;
use objc2_core_graphics::CGEvent;
use objc2_core_graphics::CGEventTapLocation;
use objc2_core_graphics::CGEventTapOptions;
use objc2_core_graphics::CGEventTapPlacement;
use objc2_core_graphics::CGEventTapProxy;
use objc2_core_graphics::CGEventType;
use std::ffi::c_void;
use std::ptr::NonNull;

pub struct InputEventListener {
    port: CFRetained<CFMachPort>,
}

extern "C-unwind" fn callback(
    _: CGEventTapProxy,
    event_type: CGEventType,
    mut event_ref: NonNull<CGEvent>,
    _listener: *mut c_void,
) -> *mut CGEvent {
    // this should cover both clicking the close button and closing out something
    // by using cmd+w or cmd+q since some apps don't send events on close
    reaper::send_notification(ReaperNotification::MouseUpKeyUp);

    // this one is only really for when people "drag" a tab out of one window
    // to create another window - we wanna make sure it gets handled because
    // events don't get sent sometimes
    if event_type == CGEventType::LeftMouseUp
        && let Some(element) = MacosApi::foreground_window()
    {
        let mut pid = 0;
        unsafe {
            element.pid(NonNull::from_mut(&mut pid));
        }

        if pid != 0
            && let Some(event) = WindowManagerEvent::from_system_notification(
                SystemNotification::Manual(ManualNotification::ShowOnInputEvent),
                pid,
                AccessibilityApi::window_id(&element).ok(),
            )
        {
            window_manager_event_listener::send_notification(event);
        }
    }

    unsafe { event_ref.as_mut() }
}

impl InputEventListener {
    pub fn init(run_loop: &CFRunLoop) -> Option<Self> {
        let mouse_event_mask = (1 << CGEventType::LeftMouseUp.0) | (1 << CGEventType::KeyUp.0);
        let mut port = None;

        unsafe {
            let tap_port = CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                mouse_event_mask,
                Some(callback),
                std::ptr::null_mut(),
            );

            if let Some(tap_port) = &tap_port {
                port = Some(tap_port.clone());
            }

            match CFMachPort::new_run_loop_source(None, tap_port.as_deref(), 0) {
                None => {}
                Some(source) => {
                    CFRunLoop::add_source(run_loop, Some(&source), kCFRunLoopDefaultMode);
                }
            }
        }

        port.map(|port| Self { port })
    }
}

impl Drop for InputEventListener {
    fn drop(&mut self) {
        CFMachPort::invalidate(&self.port);
        CGEvent::tap_enable(&self.port, false);
    }
}

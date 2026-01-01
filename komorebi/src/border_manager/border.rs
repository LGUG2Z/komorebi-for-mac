use crate::AccessibilityObserver;
use crate::AccessibilityUiElement;
use crate::CoreFoundationRunLoop;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::notification_constants::kAXMainWindowChangedNotification;
use crate::accessibility::notification_constants::kAXWindowMovedNotification;
use crate::accessibility::notification_constants::kAXWindowResizedNotification;
use crate::border_manager::BORDER_OFFSET;
use crate::border_manager::BORDER_WIDTH;
use crate::border_manager::ns_window::NsWindow;
use crate::border_manager::window_kind_colour;
use crate::core::WindowKind;
use crate::core::rect::Rect;
use crate::core_graphics::CoreGraphicsApi;
use crate::macos_api::MacosApi;
use color_eyre::eyre;
use dispatch2::DispatchQueue;
use komorebi_themes::colour::Rgb;
use objc2::rc::Retained;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWindow;
use objc2_application_services::AXObserver;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFString;
use objc2_core_foundation::CGFloat;
use objc2_core_graphics::CGMainDisplayID;
use objc2_foundation::NSPoint;
use objc2_foundation::NSRect;
use objc2_quartz_core::CATransaction;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use tracing::instrument;

#[instrument(skip_all)]
unsafe extern "C-unwind" fn border_observer_callback(
    _observer: NonNull<AXObserver>,
    _element: NonNull<AXUIElement>,
    _notification: NonNull<CFString>,
    context: *mut c_void,
) {
    unsafe {
        if !context.is_null() {
            let border = &*context.cast::<Border>();
            if !border.ns_window.window.isVisible() {
                return;
            }

            if let Ok(rect) = MacosApi::window_rect(&border.tracking_element) {
                let frame = Rect::from(CoreGraphicsApi::display_bounds(CGMainDisplayID()));
                let mut ns_rect = NSRect::new(
                    NSPoint::new(
                        rect.origin.x,
                        frame.bottom as CGFloat - rect.origin.y - rect.size.height,
                    ),
                    rect.size,
                );

                let offset = BORDER_OFFSET.load(Ordering::Relaxed) as f64;

                ns_rect.origin.x -= offset;
                ns_rect.origin.y -= offset;
                ns_rect.size.width += offset * 2.0;
                ns_rect.size.height += offset * 2.0;

                border.update();
                border
                    .ns_window
                    .window
                    .setFrame_display_animate(ns_rect, false, false);
            }
        }
    }
}

#[derive(Debug)]
pub struct Border {
    pub id: String,
    #[allow(dead_code)]
    // we need to keep a reference to this so it stays alive
    pub observer: AccessibilityObserver,
    pub tracking_element: AccessibilityUiElement,
    pub tracking_window_id: u32,
    pub process_id: i32,
    pub monitor_idx: Option<usize>,
    pub ns_window: NsWindow,
    pub window_kind: WindowKind,
}

unsafe impl Send for Border {}

impl Border {
    pub fn create(
        id: &str,
        tracking_window_id: u32,
        process_id: i32,
        element: AccessibilityUiElement,
        monitor_idx: Option<usize>,
        run_loop: CoreFoundationRunLoop,
    ) -> eyre::Result<Box<Self>> {
        let observer = AccessibilityObserver(Some(AccessibilityApi::create_observer(
            process_id,
            Some(border_observer_callback),
        )?));

        let rect = MacosApi::window_rect(&element).unwrap_or_default();

        let frame = Rect::from(CoreGraphicsApi::display_bounds(CGMainDisplayID()));
        let ns_rect = NSRect::new(
            NSPoint::new(
                rect.origin.x,
                frame.bottom as CGFloat - rect.origin.y - rect.size.height,
            ),
            rect.size,
        );

        let mut border = Box::new(Self {
            id: id.to_string(),
            tracking_window_id,
            process_id,
            monitor_idx,
            observer: observer.clone(),
            tracking_element: element.clone(),
            ns_window: NsWindow::new(ns_rect, tracking_window_id)?,
            window_kind: WindowKind::Unfocused,
        });

        DispatchQueue::main().exec_sync(|| {
            let border_ptr = std::ptr::addr_of_mut!(*border).cast::<c_void>();
            if let Err(error) = AccessibilityApi::add_observer_to_run_loop(
                &observer,
                &element,
                &[
                    kAXWindowMovedNotification,
                    kAXWindowResizedNotification,
                    kAXMainWindowChangedNotification,
                ],
                &run_loop,
                Some(border_ptr),
            ) {
                tracing::warn!("failed to create border observer: {error}")
            }
        });

        Ok(border)
    }

    pub fn update_tracking_element(
        &mut self,
        new_element: AccessibilityUiElement,
        border_ptr: *mut c_void,
    ) {
        let old_element = &self.tracking_element;
        let notifications = [
            kAXWindowMovedNotification,
            kAXWindowResizedNotification,
            kAXMainWindowChangedNotification,
        ];

        // re-register observer notifications on the new element
        if let Some(ref observer) = self.observer.0 {
            for notification in &notifications {
                let _ = AccessibilityApi::remove_notification_from_observer(
                    observer,
                    &old_element.0,
                    notification,
                );
            }

            for notification in &notifications {
                let _ = AccessibilityApi::add_notification_to_observer(
                    observer,
                    &new_element.0,
                    notification,
                    Some(border_ptr),
                );
            }
        }

        self.tracking_element = new_element;
    }

    pub fn update(&self) {
        autoreleasepool(|_| {
            let colour = Rgb::from(window_kind_colour(self.window_kind));

            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.ns_window.set_border_color(colour);
            self.ns_window
                .set_border_width(BORDER_WIDTH.load(Ordering::Relaxed) as f64);
            // TODO: why does this crash?
            // self.ns_window.window.setFrame_display(ns_rect, true);
            CATransaction::commit();
        })
    }

    pub fn destroy(&self) {
        let window_ptr = Retained::as_ptr(&self.ns_window.window) as usize;

        DispatchQueue::main().exec_sync(|| {
            autoreleasepool(|_| unsafe {
                let window = window_ptr as *const NSWindow;
                (*window).close();
            });
        });
    }
}

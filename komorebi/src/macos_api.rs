use crate::LibraryError;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::attribute_constants::kAXFocusedApplicationAttribute;
use crate::accessibility::attribute_constants::kAXFocusedWindowAttribute;
use crate::accessibility::attribute_constants::kAXPositionAttribute;
use crate::accessibility::attribute_constants::kAXSizeAttribute;
use crate::accessibility::error::AccessibilityApiError;
use crate::accessibility::error::AccessibilityError;
use crate::application::Application;
use crate::cf_array_as;
use crate::container::Container;
use crate::core::rect::Rect;
use crate::core_graphics::CoreGraphicsApi;
use crate::monitor::Monitor;
use crate::window::WindowInfo;
use crate::window_manager::WindowManager;
use color_eyre::eyre;
use objc2::MainThreadMarker;
use objc2_app_kit::NSEvent;
use objc2_app_kit::NSScreen;
use objc2_application_services::AXUIElement;
use objc2_application_services::AXValue;
use objc2_application_services::AXValueType;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFString;
use objc2_core_foundation::CGPoint;
use objc2_core_foundation::CGRect;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

pub struct MacosApi;

impl MacosApi {
    pub fn load_monitor_information(wm: &mut WindowManager) -> Result<(), LibraryError> {
        let screens = NSScreen::screens(MainThreadMarker::new().unwrap());

        for display_id in CoreGraphicsApi::connected_display_ids()? {
            let display_bounds = CoreGraphicsApi::display_bounds(display_id);

            for screen in &screens {
                let menu_bar_height = screen.frame().size.height
                    - screen.visibleFrame().size.height
                    - screen.visibleFrame().origin.y;

                if screen.frame() == display_bounds {
                    let size = Rect::from(display_bounds);
                    let mut work_area_size = Rect::from(display_bounds);
                    work_area_size.top += menu_bar_height as i32;
                    work_area_size.bottom = screen.visibleFrame().size.height as i32;

                    let monitor = Monitor::new(display_id, size, work_area_size);
                    wm.monitors.elements_mut().push_back(monitor);
                }
            }
        }

        Ok(())
    }

    pub fn update_monitor_work_areas(wm: Arc<Mutex<WindowManager>>) -> eyre::Result<()> {
        let screens = NSScreen::screens(MainThreadMarker::new().unwrap());

        for display_id in CoreGraphicsApi::connected_display_ids()? {
            let display_bounds = CoreGraphicsApi::display_bounds(display_id);

            let mut wm = wm.lock();

            for screen in &screens {
                let menu_bar_height = screen.frame().size.height
                    - screen.visibleFrame().size.height
                    - screen.visibleFrame().origin.y;

                if screen.frame() == display_bounds {
                    let size = Rect::from(display_bounds);
                    let mut work_area_size = Rect::from(display_bounds);
                    work_area_size.top += menu_bar_height as i32;
                    work_area_size.bottom = screen.visibleFrame().size.height as i32;

                    for monitor in wm.monitors_mut() {
                        if monitor.id == display_id {
                            monitor.size = size;
                            monitor.work_area_size = work_area_size;
                        }

                        tracing::info!(
                            "updated monitor size and work area for monitor {display_id}"
                        )
                    }
                }
            }

            let focus_follows_mouse = wm.mouse_follows_focus;
            wm.update_focused_workspace(focus_follows_mouse, true)?;
        }

        Ok(())
    }

    pub fn load_workspace_information(wm: &mut WindowManager) -> Result<(), LibraryError> {
        let mut monitor_size_map = HashMap::new();
        let mut monitor_workspace_map = HashMap::new();
        let mut monitor_window_map = HashMap::new();
        let mut valid_window_count = 0;

        for (idx, monitor) in wm.monitors.elements_mut().iter_mut().enumerate() {
            monitor_size_map.insert(idx, monitor.size);
            monitor_workspace_map.insert(idx, monitor.focused_workspace_mut());
        }

        if let Some(window_list_info) = CoreGraphicsApi::window_list_info() {
            tracing::info!("{} windows found", window_list_info.len());

            for raw_window_info in cf_array_as::<CFDictionary>(&window_list_info) {
                if let Some(info) = WindowInfo::new(raw_window_info).validated() {
                    let window_rect = Rect::from(info.bounds);

                    for (monitor_idx, monitor_size) in &monitor_size_map {
                        if monitor_size.contains(&window_rect) {
                            let entry = monitor_window_map
                                .entry(monitor_idx)
                                .or_insert_with(Vec::new);

                            let application = match wm.applications.entry(info.owner_pid) {
                                Entry::Occupied(entry) => entry.into_mut(),
                                Entry::Vacant(vacant) => {
                                    let mut application = Application::new(info.owner_pid)?;
                                    // TODO: this ain't great, fix this OBS workaround
                                    application.observe(&wm.run_loop);
                                    vacant.insert(application)
                                }
                            };

                            if let Some(window) = application.window_by_title(&info.name) {
                                window.observe(&wm.run_loop)?;
                                entry.push(window);
                                valid_window_count += 1
                            }
                        }
                    }
                }
            }

            tracing::info!("{valid_window_count} valid windows identified");
        }

        for (monitor_idx, windows) in monitor_window_map {
            for window in windows {
                let mut container = Container::default();

                container.windows_mut().push_back(window);
                if let Some(Some(workspace)) = monitor_workspace_map.get_mut(monitor_idx) {
                    workspace.containers_mut().push_back(container);
                }
            }
        }

        Ok(())
    }

    pub fn center_cursor_in_rect(rect: &Rect) -> Result<(), LibraryError> {
        Ok(CoreGraphicsApi::warp_mouse_cursor_position(
            rect.left + (rect.right / 2),
            rect.top + (rect.bottom / 2),
        )?)
    }

    pub fn window_rect(element: &AXUIElement) -> Result<CGRect, AccessibilityError> {
        let mut position_receiver = std::ptr::null();
        let mut size_receiver = std::ptr::null();

        unsafe {
            match AccessibilityApiError::from(element.copy_attribute_value(
                &CFString::from_static_str(kAXPositionAttribute),
                NonNull::from_mut(&mut position_receiver),
            )) {
                AccessibilityApiError::Success => {}
                error => {
                    tracing::error!("failed to get window position: {error}");
                    return Err(error.into());
                }
            };

            match AccessibilityApiError::from(element.copy_attribute_value(
                &CFString::from_static_str(kAXSizeAttribute),
                NonNull::from_mut(&mut size_receiver),
            )) {
                AccessibilityApiError::Success => {}
                error => {
                    tracing::error!("failed to get window size: {error}");
                    return Err(error.into());
                }
            };

            let mut rect = CGRect::default();

            AXValue::value(
                &*position_receiver.cast::<AXValue>(),
                AXValueType::CGPoint,
                NonNull::from_mut(&mut rect.origin).cast::<c_void>(),
            );
            AXValue::value(
                &*size_receiver.cast::<AXValue>(),
                AXValueType::CGSize,
                NonNull::from_mut(&mut rect.size).cast::<c_void>(),
            );

            Ok(rect)
        }
    }

    pub fn foreground_window_id() -> Option<u32> {
        unsafe {
            let syswide = AXUIElement::new_system_wide();
            let app = AccessibilityApi::copy_attribute_value::<AXUIElement>(
                &syswide,
                kAXFocusedApplicationAttribute,
            )?;

            let window = AccessibilityApi::copy_attribute_value::<AXUIElement>(
                &app,
                kAXFocusedWindowAttribute,
            )?;

            AccessibilityApi::window_id(&window).ok()
        }
    }

    pub fn foreground_window() -> Option<CFRetained<AXUIElement>> {
        unsafe {
            let syswide = AXUIElement::new_system_wide();
            let app = AccessibilityApi::copy_attribute_value::<AXUIElement>(
                &syswide,
                kAXFocusedApplicationAttribute,
            )?;

            AccessibilityApi::copy_attribute_value::<AXUIElement>(&app, kAXFocusedWindowAttribute)
        }
    }

    pub fn cursor_pos() -> CGPoint {
        unsafe {
            let point = NSEvent::mouseLocation();
            CGPoint::new(point.x, point.y)
        }
    }

    pub fn monitor_from_point(point: CGPoint) -> Option<u32> {
        CoreGraphicsApi::display_with_point(point)
    }
}

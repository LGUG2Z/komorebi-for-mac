use crate::LibraryError;
use crate::accessibility::attribute_constants::kAXPositionAttribute;
use crate::accessibility::attribute_constants::kAXSizeAttribute;
use crate::application::Application;
use crate::cf_array_as;
use crate::core::rect::Rect;
use crate::core_graphics::CoreGraphicsApi;
use crate::monitor::Monitor;
use crate::window::WindowInfo;
use crate::window_manager::Container;
use crate::window_manager::WindowManager;
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFString;
use objc2_core_foundation::CGPoint;
use objc2_core_foundation::CGSize;
use std::collections::HashMap;
use std::ptr::NonNull;

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

                            let application =
                                wm.applications.entry(info.owner_pid).or_insert_with(|| {
                                    let application = Application::new(info.owner_pid)
                                        .unwrap_or_else(|_| {
                                            panic!(
                                                "failed to create application from pid {}",
                                                info.owner_pid
                                            )
                                        });
                                    application
                                        .observe(&wm.run_loop)
                                        .expect("application must be observable");
                                    application
                                });

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

    pub fn window_rect(element: &AXUIElement) -> Rect {
        let mut position_receiver = std::ptr::null();
        let mut size_receiver = std::ptr::null();

        unsafe {
            element.copy_attribute_value(
                &CFString::from_static_str(kAXPositionAttribute),
                NonNull::from(&mut position_receiver),
            );

            element.copy_attribute_value(
                &CFString::from_static_str(kAXSizeAttribute),
                NonNull::from(&mut size_receiver),
            );

            let position = *position_receiver.cast::<CGPoint>();
            let size = *position_receiver.cast::<CGSize>();

            Rect {
                left: position.x as i32,
                top: position.y as i32,
                right: size.width as i32,
                bottom: size.height as i32,
            }
        }
    }
}

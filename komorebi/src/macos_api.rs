use crate::LibraryError;
use crate::application::Application;
use crate::cf_array_as;
use crate::core_graphics::CoreGraphicsApi;
use crate::rect::Rect;
use crate::ring::Ring;
use crate::window::WindowInfo;
use crate::window_manager::Container;
use crate::window_manager::Monitor;
use crate::window_manager::WindowManager;
use objc2_core_foundation::CFDictionary;
use std::collections::HashMap;

pub struct MacosApi;

impl MacosApi {
    pub fn load_monitor_information(wm: &mut WindowManager) -> Result<(), LibraryError> {
        for display_id in CoreGraphicsApi::connected_display_ids()? {
            let size = Rect::from(CoreGraphicsApi::display_bounds(display_id));
            let monitor = Monitor::new(display_id, size);
            wm.monitors.elements_mut().push_back(monitor);
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
                let mut container = Container {
                    windows: Ring::default(),
                };

                container.windows_mut().push_back(window);
                if let Some(Some(workspace)) = monitor_workspace_map.get_mut(monitor_idx) {
                    workspace.containers_mut().push_back(container);
                }
            }
        }

        Ok(())
    }
}

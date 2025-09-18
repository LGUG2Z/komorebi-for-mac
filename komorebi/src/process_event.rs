use crate::accessibility::AccessibilityApi;
use crate::accessibility::notification_constants::AccessibilityNotification;
use crate::window::Window;
use crate::window_manager::WindowManager;
use crate::window_manager_event::WindowManagerEvent;
use color_eyre::eyre;
use parking_lot::Mutex;
use std::sync::Arc;

#[tracing::instrument]
pub fn listen_for_events(wm: Arc<Mutex<WindowManager>>) {
    let receiver = wm.lock().incoming_events.clone();

    std::thread::spawn(move || {
        tracing::info!("listening");
        loop {
            if let Ok(event) = receiver.recv() {
                let mut guard = wm.lock();
                match guard.process_event(event) {
                    Ok(()) => {}
                    Err(error) => {
                        if cfg!(debug_assertions) {
                            tracing::error!("{:?}", error)
                        } else {
                            tracing::error!("{}", error)
                        }
                    }
                }
            }
        }
    });
}

impl WindowManager {
    pub fn process_event(&mut self, event: WindowManagerEvent) -> eyre::Result<()> {
        if self.is_paused {
            tracing::trace!("ignoring while paused");
            return Ok(());
        }

        tracing::info!("processing event: {event}");

        match event {
            WindowManagerEvent::FocusChange(notification, process_id, _) => {
                let application = self.application(process_id)?;

                if let Some(window_id) = application.main_window_id() {
                    self.focused_workspace_mut()?
                        .focus_container_by_window(window_id)?;
                }

                if matches!(notification, AccessibilityNotification::AXMainWindowChanged) {
                    self.reap_invalid_windows_for_application(process_id)?;
                    self.update_focused_workspace(false, false)?;
                }
            }
            WindowManagerEvent::Show(_, process_id) => {
                let mut window_id = None;
                let mut window_element = None;
                let mut create = true;

                {
                    let application = self.application(process_id)?;
                    if let Some(element) = application.main_window()
                        && let Ok(wid) = AccessibilityApi::window_id(&element)
                    {
                        window_id = Some(wid);
                        window_element = Some(element.clone());
                    }
                }

                if let Some(window_id) = window_id {
                    let workspace = self.focused_workspace()?;
                    if workspace.contains_window(window_id) {
                        // ignore bogus show events
                        tracing::debug!("ignoring show event for window already on workspace");
                        create = false;
                    }
                }

                if create {
                    let application = self.application(process_id)?;
                    if let Some(element) = window_element
                        && let Ok(window) = Window::new(element, application.clone())
                    {
                        window.observe(&self.run_loop)?;

                        let workspace = self.focused_workspace_mut()?;
                        workspace.new_container_for_window(&window)?;

                        self.update_focused_workspace(false, false)?;
                    }
                }
            }
            WindowManagerEvent::Destroy(_, process_id) => {
                self.reap_invalid_windows_for_application(process_id)?;
                self.update_focused_workspace(false, false)?;
            }
            WindowManagerEvent::Minimize(_, _, window_id) => {
                self.extract_minimized_window(window_id)?;
                self.update_focused_workspace(false, false)?;
            }
            WindowManagerEvent::Restore(_, _, window_id) => {
                match self.minimized_windows.remove(&window_id) {
                    None => {}
                    Some(window) => {
                        let workspace = self.focused_workspace_mut()?;
                        workspace.new_container_for_window(&window)?;
                        self.update_focused_workspace(false, false)?;
                    }
                }
            }
        }

        Ok(())
    }
}

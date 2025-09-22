use crate::accessibility::AccessibilityApi;
use crate::accessibility::notification_constants::AccessibilityNotification;
use crate::ax_event_listener::event_tx;
use crate::core::default_layout::DefaultLayout;
use crate::core::layout::Layout;
use crate::window::Window;
use crate::window_manager::WindowManager;
use crate::window_manager_event::SystemNotification;
use crate::window_manager_event::WindowManagerEvent;
use crate::workspace::WorkspaceLayer;
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

        let mut should_manage = true;
        {
            let application = self.application(event.process_id())?;
            if let Some(window_element) = application.main_window()
                && let Ok(window) = Window::new(window_element, application.clone())
            {
                let print_window = window.clone();
                should_manage = window.should_manage(Some(event))?;

                if !should_manage {
                    tracing::debug!(
                        "ignoring event as window should not be managed: {print_window}"
                    );
                }
            }
        }

        if !should_manage {
            return Ok(());
        }

        tracing::info!(
            "processing event: {event} for process {}",
            event.process_id()
        );

        self.enforce_workspace_rules()?;

        match event {
            WindowManagerEvent::FocusChange(notification, process_id, _) => {
                let application = self.application(process_id)?;

                if let Some(window_id) = application.main_window_id() {
                    // TODO: figure out if this applies on macOS too
                    // don't want to trigger the full workspace updates when there are no managed
                    // containers - this makes floating windows on empty workspaces go into very
                    // annoying focus change loops which prevents users from interacting with them
                    if !matches!(
                        self.focused_workspace()?.layout,
                        Layout::Default(DefaultLayout::Scrolling)
                    ) && !self.focused_workspace()?.containers().is_empty()
                    {
                        self.update_focused_workspace(self.mouse_follows_focus, false)?;
                    }

                    let workspace = self.focused_workspace_mut()?;
                    let floating_window_idx = workspace
                        .floating_windows()
                        .iter()
                        .position(|w| w.id == window_id);

                    match floating_window_idx {
                        None => {
                            // if let Some(w) = workspace.maximized_window() {
                            //     if w.hwnd == window_id {
                            //         return Ok(());
                            //     }
                            // }

                            if let Some(monocle) = &workspace.monocle_container {
                                if let Some(window) = monocle.focused_window() {
                                    window.focus(false)?;
                                }
                            } else {
                                // if this fails, the app was probably open but windowless when komorebi
                                // launched, so the window hasn't been registered - we should treat it
                                // as a "Show" event
                                if workspace.focus_container_by_window(window_id).is_err() {
                                    event_tx().send(WindowManagerEvent::Show(
                                        notification,
                                        event.process_id(),
                                    ))?
                                }
                            }

                            workspace.layer = WorkspaceLayer::Tiling;

                            if matches!(
                                self.focused_workspace()?.layout,
                                Layout::Default(DefaultLayout::Scrolling)
                            ) && !self.focused_workspace()?.containers().is_empty()
                            {
                                self.update_focused_workspace(self.mouse_follows_focus, false)?;
                            }
                        }
                        Some(idx) => {
                            if let Some(_window) = workspace.floating_windows().get(idx) {
                                workspace.layer = WorkspaceLayer::Floating;
                            }
                        }
                    }
                }

                if matches!(
                    notification,
                    SystemNotification::Accessibility(
                        AccessibilityNotification::AXMainWindowChanged
                    )
                ) {
                    self.reap_invalid_windows_for_application(process_id)?;
                    self.update_focused_workspace(false, false)?;
                }
            }
            // TODO: upate this to work with floating applications / rules
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

use crate::TABBED_APPLICATIONS;
use crate::WORKSPACE_MATCHING_RULES;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::notification_constants::AccessibilityNotification;
use crate::ax_event_listener::event_tx;
use crate::core::config_generation::MatchingRule;
use crate::core::default_layout::DefaultLayout;
use crate::core::layout::Layout;
use crate::macos_api::MacosApi;
use crate::window::Window;
use crate::window_manager::WindowManager;
use crate::window_manager_event::SystemNotification;
use crate::window_manager_event::WindowManagerEvent;
use crate::workspace::WorkspaceLayer;
use color_eyre::eyre;
use parking_lot::Mutex;
use std::sync::Arc;
use tracing::instrument;

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
    #[instrument(skip_all)]
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
            "processing event: {event} for process {} with notification {}",
            event.process_id(),
            event.notification(),
        );

        self.enforce_workspace_rules()?;

        match event {
            WindowManagerEvent::FocusChange(notification, process_id, _) => {
                let application = self.application(process_id)?;
                let application_name = application.name().unwrap_or_default().clone();
                let mut tabbed_window = false;

                if let Some(window_id) = application.main_window_id() {
                    if matches!(
                        notification,
                        SystemNotification::Accessibility(
                            AccessibilityNotification::AXMainWindowChanged
                        )
                    ) && let Some(element) = application.main_window()
                    {
                        let workspace = self.focused_workspace_mut()?;

                        let tabbed_applications = TABBED_APPLICATIONS.lock();
                        if tabbed_applications.contains(&application_name) {
                            for window in workspace.visible_windows().iter().flatten() {
                                if window.application.name().unwrap_or_default() == application_name
                                {
                                    let tab_rect = MacosApi::window_rect(&element)?;
                                    let main_rect = MacosApi::window_rect(&window.element)?;
                                    if tab_rect == main_rect {
                                        tracing::debug!("ignoring focus change for tabbed window");
                                        tabbed_window = true;
                                    }
                                }
                            }
                        }

                        drop(tabbed_applications);
                    }

                    if !tabbed_window {
                        let is_known = self.known_window_ids.get(&window_id).cloned();
                        let mut is_on_current_workspace = false;

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
                        if workspace.contains_window(window_id) {
                            is_on_current_workspace = true;
                        }

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
                                } else if !is_on_current_workspace && is_known.is_none() {
                                    // thanks, I hate it - need to do this so that we don't mess up
                                    // the workspace rules, but also don't miss events from dumb
                                    // apps like notes, mail etc.
                                    let mut has_matching_workspace_rule = false;
                                    let workspace_rules = WORKSPACE_MATCHING_RULES.lock();
                                    for rule in &*workspace_rules {
                                        match &rule.matching_rule {
                                            MatchingRule::Simple(r) => {
                                                if r.id.trim_end_matches(".exe") == application_name
                                                {
                                                    has_matching_workspace_rule = true;
                                                }
                                            }
                                            // TODO: this is pretty coarse
                                            MatchingRule::Composite(rules) => {
                                                for r in rules {
                                                    if r.id.trim_end_matches(".exe")
                                                        == application_name
                                                    {
                                                        has_matching_workspace_rule = true;
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if !has_matching_workspace_rule
                                        && workspace.focus_container_by_window(window_id).is_err()
                                    {
                                        // if this fails, the app was probably open but windowless when komorebi
                                        // launched, so the window hasn't been registered - we should treat it
                                        // as a "Show" event
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

                        // maybe we don't need a separate reconciliator module?? this works pretty well!
                        if !is_on_current_workspace && let Some((m_idx, w_idx)) = is_known {
                            self.focus_monitor(m_idx)?;
                            self.focus_workspace(w_idx)?;
                        }
                    }
                }

                if matches!(
                    notification,
                    SystemNotification::Accessibility(
                        AccessibilityNotification::AXMainWindowChanged
                    )
                ) && !tabbed_window
                {
                    self.reap_invalid_windows_for_application(process_id)?;
                    self.update_focused_workspace(false, false)?;
                }
            }
            // TODO: update this to work with floating applications / rules
            WindowManagerEvent::Show(_, process_id) => {
                let mut window_id = None;
                let mut window_element = None;
                let mut application_name = String::new();
                let mut tabbed_window = false;
                let mut create = true;

                {
                    let application = self.application(process_id)?;
                    if let Some(element) = application.main_window()
                        && let Ok(wid) = AccessibilityApi::window_id(&element)
                    {
                        window_id = Some(wid);
                        window_element = Some(element.clone());
                        application_name = application.name().unwrap_or_default().clone();
                    }
                }

                if let (Some(window_id), Some(element)) = (window_id, &window_element) {
                    let workspace = self.focused_workspace()?;

                    let tabbed_applications = TABBED_APPLICATIONS.lock();
                    if tabbed_applications.contains(&application_name) {
                        for window in workspace.visible_windows().iter().flatten() {
                            if window.application.name().unwrap_or_default() == application_name {
                                let tab_rect = MacosApi::window_rect(element)?;
                                let main_rect = MacosApi::window_rect(&window.element)?;
                                if tab_rect == main_rect {
                                    tabbed_window = true;
                                }
                            }
                        }
                    }

                    drop(tabbed_applications);

                    if workspace.contains_window(window_id) {
                        // ignore bogus show events
                        tracing::debug!("ignoring show event for window already on workspace");
                        create = false;
                    }

                    if let Some((m_idx, w_idx)) = self.known_window_ids.get(&window_id)
                        && let Some(focused_workspace_idx) = self
                            .monitors()
                            .get(*m_idx)
                            .map(|m| m.focused_workspace_idx())
                        && *m_idx != self.focused_monitor_idx()
                        && *w_idx != focused_workspace_idx
                    {
                        tracing::debug!(
                            "ignoring show event for window already associated with another workspace"
                        );

                        Window::hide_adhoc(window_id, element)?;
                        create = false;
                    }
                }

                if create && !tabbed_window {
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

        self.enforce_workspace_rules()?;
        self.update_known_window_ids();

        Ok(())
    }
}

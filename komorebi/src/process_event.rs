use crate::FLOATING_APPLICATIONS;
use crate::Notification;
use crate::NotificationEvent;
use crate::REGEX_IDENTIFIERS;
use crate::TABBED_APPLICATIONS;
use crate::WORKSPACE_MATCHING_RULES;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::error::AccessibilityApiError;
use crate::accessibility::error::AccessibilityError;
use crate::accessibility::notification_constants::AccessibilityNotification;
use crate::core::WindowContainerBehaviour;
use crate::core::config_generation::MatchingRule;
use crate::core::default_layout::DefaultLayout;
use crate::core::layout::Layout;
use crate::macos_api::MacosApi;
use crate::notify_subscribers;
use crate::state::State;
use crate::window::AdhocWindow;
use crate::window::Window;
use crate::window::should_act;
use crate::window_manager::WindowManager;
use crate::window_manager_event::ManualNotification;
use crate::window_manager_event::SystemNotification;
use crate::window_manager_event::WindowManagerEvent;
use crate::window_manager_event_listener;
use crate::workspace::WorkspaceLayer;
use color_eyre::eyre;
use color_eyre::eyre::OptionExt;
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

        // don't want to spam logs for manually triggered hacks triggered
        // from the input listener
        if !matches!(
            event,
            WindowManagerEvent::Show(SystemNotification::Manual(_), _)
        ) {
            tracing::info!(
                "processing event: {event} for process {} with notification {}",
                event.process_id(),
                event.notification(),
            );
        }

        #[allow(clippy::useless_asref)]
        // We don't have From implemented for &mut WindowManager
        let initial_state = State::from(self.as_ref());

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
                            let mut first_tab_destroyed = false;

                            for window in workspace.visible_windows() {
                                if let Some(window) = window
                                    && window.application.name().unwrap_or_default()
                                        == application_name
                                {
                                    let tab_rect = MacosApi::window_rect(&element)?;
                                    let main_rect = match MacosApi::window_rect(&window.element) {
                                        Ok(rect) => rect,
                                        Err(AccessibilityError::Api(
                                            AccessibilityApiError::InvalidUIElement,
                                        )) => {
                                            // this means we have closed the 1st tab, so we need this window object to be reaped
                                            // and for the new 1st tab to be the key element of the window struct
                                            if let Some(event) =
                                                WindowManagerEvent::from_system_notification(
                                                    SystemNotification::Manual(
                                                        ManualNotification::ShowOnFocusChangeFirstTabDestroyed,
                                                    ),
                                                    event.process_id(),
                                                    Some(window_id),
                                                )
                                            {
                                                window_manager_event_listener::send_notification(
                                                    event,
                                                );
                                            }

                                            first_tab_destroyed = true;

                                            tracing::debug!(
                                                "first tab of a native tabbed app was destroyed; reaping window and sending a new show event"
                                            );

                                            tab_rect
                                        }
                                        Err(error) => return Err(error.into()),
                                    };

                                    if tab_rect == main_rect {
                                        tracing::debug!("ignoring focus change for tabbed window");
                                        tabbed_window = true;
                                    }
                                }
                            }

                            if first_tab_destroyed {
                                self.reap_invalid_windows_for_application(process_id)?;
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
                                        if let Some(event) =
                                            WindowManagerEvent::from_system_notification(
                                                SystemNotification::Manual(
                                                    ManualNotification::ShowOnFocusChangeWindowlessAppRestored,
                                                ),
                                                event.process_id(),
                                                Some(window_id),
                                            )
                                        {
                                            window_manager_event_listener::send_notification(
                                                event,
                                            );
                                        }
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
                let focused_monitor_idx = self.focused_monitor_idx();
                let focused_workspace_idx =
                    self.focused_workspace_idx_for_monitor_idx(focused_monitor_idx)?;

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
                        if !matches!(
                            event,
                            WindowManagerEvent::Show(SystemNotification::Manual(_), _)
                        ) {
                            // don't want to spam logs for manually triggered hacks triggered
                            // from the input listener
                            tracing::debug!("ignoring show event for window already on workspace");
                        }

                        // ignore bogus show events
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

                        AdhocWindow::hide(window_id, element)?;
                        create = false;
                    }
                }

                if create && !tabbed_window {
                    let application = self.application(process_id)?;
                    if let Some(element) = window_element
                        && let Ok(mut window) = Window::new(element, application.clone())
                    {
                        window.observe(&self.run_loop)?;

                        let behaviour = self.window_management_behaviour(
                            focused_monitor_idx,
                            focused_workspace_idx,
                        );
                        let workspace = self.focused_workspace_mut()?;
                        let workspace_contains_window = workspace.contains_window(window.id);
                        let monocle_container = workspace.monocle_container.clone();

                        let floating_applications = FLOATING_APPLICATIONS.lock();
                        let mut should_float = false;

                        if !floating_applications.is_empty() {
                            let regex_identifiers = REGEX_IDENTIFIERS.lock();

                            if let (
                                Some(title),
                                Some(exe_name),
                                Some(role),
                                Some(subrole),
                                Some(path),
                            ) = (
                                window.title(),
                                window.exe(),
                                window.role(),
                                window.subrole(),
                                window.path(),
                            ) {
                                should_float = should_act(
                                    &title,
                                    &exe_name,
                                    &[&role, &subrole],
                                    &path.to_string_lossy(),
                                    &floating_applications,
                                    &regex_identifiers,
                                )
                                .is_some();
                            }
                        }

                        if behaviour.float_override
                            || behaviour.floating_layer_override
                            // TODO: look into if we want to have a Manage command here too
                            || (should_float /*&& !matches!(event, WindowManagerEvent::Manage(_))*/ )
                        {
                            let placement = if behaviour.floating_layer_override {
                                // Floating layer override placement
                                behaviour.floating_layer_placement
                            } else if behaviour.float_override {
                                // Float override placement
                                behaviour.float_override_placement
                            } else {
                                // Float rule placement
                                behaviour.float_rule_placement
                            };
                            // Center floating windows according to the proper placement if not
                            // on a floating workspace
                            let center_spawned_floats = placement.should_center() && workspace.tile;
                            workspace.floating_windows_mut().push_back(window.clone());
                            workspace.layer = WorkspaceLayer::Floating;
                            if center_spawned_floats {
                                let mut floating_window = window.clone();
                                floating_window.center(
                                    &workspace.globals.work_area,
                                    placement.should_resize(),
                                )?;
                            }

                            self.update_focused_workspace(false, false)?;
                        } else {
                            match behaviour.current_behaviour {
                                WindowContainerBehaviour::Create => {
                                    workspace.new_container_for_window(&window)?;
                                    workspace.layer = WorkspaceLayer::Tiling;
                                    self.update_focused_workspace(false, false)?;
                                }
                                WindowContainerBehaviour::Append => {
                                    workspace
                                        .focused_container_mut()
                                        .ok_or_eyre("there is no focused container")?
                                        .add_window(&window)?;
                                    workspace.layer = WorkspaceLayer::Tiling;
                                    self.update_focused_workspace(true, false)?;
                                }
                            }

                            // TODO: not sure if this is needed on macOS
                            if (self.focused_workspace()?.containers().len() == 1
                                && self.focused_workspace()?.floating_windows().is_empty())
                                || (self.focused_workspace()?.containers().is_empty()
                                    && self.focused_workspace()?.floating_windows().len() == 1)
                            {
                                // If after adding this window the workspace only contains 1 window, it
                                // means it was previously empty and we focused the desktop to unfocus
                                // any previous window from other workspace, so now we need to focus
                                // this window again. This is needed because sometimes some windows
                                // first send the `FocusChange` event and only the `Show` event after
                                // and we will be focusing the desktop on the `FocusChange` event since
                                // it is still empty.
                                window.focus(self.mouse_follows_focus)?;
                            }
                        }

                        if workspace_contains_window {
                            let mut monocle_window_event = false;
                            if let Some(ref monocle) = monocle_container
                                && let Some(monocle_window) = monocle.focused_window()
                            {
                                // we should have the window_id at this point
                                if monocle_window.id == window_id.unwrap_or_default() {
                                    monocle_window_event = true;
                                }
                            }

                            let workspace = self.focused_workspace()?;
                            if !(monocle_window_event || workspace.layer != WorkspaceLayer::Tiling)
                                && monocle_container.is_some()
                            {
                                window.hide()?;
                            }
                        }

                        // let workspace = self.focused_workspace_mut()?;
                        // workspace.new_container_for_window(&window)?;
                        //
                        // self.update_focused_workspace(false, false)?;
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

        self.update_known_window_ids();

        notify_subscribers(
            Notification {
                event: NotificationEvent::WindowManager(event),
                state: self.as_ref().into(),
            },
            initial_state.has_been_modified(self.as_ref()),
        )?;

        Ok(())
    }
}

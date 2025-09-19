use crate::core::MoveBehaviour;
use crate::core::SocketMessage;
use crate::core::WindowContainerBehaviour;
use crate::core::arrangement::Axis;
use crate::core::operation_direction::OperationDirection;
use crate::core::rect::Rect;
use crate::macos_api::MacosApi;
use crate::monitor::Monitor;
use crate::window_manager::WindowManager;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceLayer;
use color_eyre::eyre;
use color_eyre::eyre::eyre;
use parking_lot::Mutex;
use std::io::BufRead;
use std::io::BufReader;
use std::num::NonZeroUsize;
use std::os::unix::net::UnixStream;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

#[tracing::instrument]
pub fn listen_for_commands(wm: Arc<Mutex<WindowManager>>) {
    std::thread::spawn(move || {
        loop {
            let wm = wm.clone();

            let _ = std::thread::spawn(move || {
                let listener = wm
                    .lock()
                    .command_listener
                    .try_clone()
                    .expect("could not clone unix listener");

                tracing::info!("listening on komorebi.sock");
                for client in listener.incoming() {
                    match client {
                        Ok(stream) => {
                            let wm_clone = wm.clone();
                            std::thread::spawn(move || {
                                match read_commands_uds(&wm_clone, stream) {
                                    Ok(()) => {}
                                    Err(error) => {
                                        tracing::error!("{error}")
                                    }
                                }
                            });
                        }
                        Err(error) => {
                            tracing::error!("failed to get unix stream {}", error);
                            break;
                        }
                    }
                }
            })
            .join();

            tracing::error!("restarting failed thread");
        }
    });
}

impl WindowManager {
    #[tracing::instrument(skip(self, _reply))]
    pub fn process_command(
        &mut self,
        message: SocketMessage,
        mut _reply: impl std::io::Write,
    ) -> eyre::Result<()> {
        tracing::info!("processing command: {message}");

        match message {
            SocketMessage::Promote => self.promote_container_to_front()?,
            SocketMessage::PromoteFocus => self.promote_focus_to_front()?,
            SocketMessage::PromoteWindow(direction) => {
                self.focus_container_in_direction(direction)?;
                self.promote_container_to_front()?
            }
            SocketMessage::FocusWindow(direction) => {
                let focused_workspace = self.focused_workspace()?;
                match focused_workspace.layer {
                    WorkspaceLayer::Tiling => {
                        self.focus_container_in_direction(direction)?;
                    }
                    WorkspaceLayer::Floating => {
                        self.focus_floating_window_in_direction(direction)?;
                    }
                }
            }
            SocketMessage::MoveWindow(direction) => {
                let focused_workspace = self.focused_workspace()?;
                match focused_workspace.layer {
                    WorkspaceLayer::Tiling => {
                        self.move_container_in_direction(direction)?;
                    }
                    WorkspaceLayer::Floating => {
                        self.move_floating_window_in_direction(direction)?;
                    }
                }
            }
            SocketMessage::StackWindow(direction) => self.add_window_to_container(direction)?,
            SocketMessage::UnstackWindow => self.remove_window_from_container()?,
            SocketMessage::CycleStack(direction) => {
                self.cycle_container_window_in_direction(direction)?;
            }
            SocketMessage::FlipLayout(layout_flip) => self.flip_layout(layout_flip)?,
            SocketMessage::ChangeLayout(layout) => self.change_workspace_layout_default(layout)?,
            SocketMessage::CycleLayout(direction) => self.cycle_layout(direction)?,
            SocketMessage::TogglePause => {
                if self.is_paused {
                    tracing::info!("resuming");
                } else {
                    tracing::info!("pausing");
                }

                self.is_paused = !self.is_paused;
            }
            SocketMessage::CycleFocusMonitor(direction) => {
                let monitor_idx = direction.next_idx(
                    self.focused_monitor_idx(),
                    NonZeroUsize::new(self.monitors().len())
                        .ok_or_else(|| eyre!("there must be at least one monitor"))?,
                );

                self.focus_monitor(monitor_idx)?;
                self.update_focused_workspace(self.mouse_follows_focus, true)?;
            }
            SocketMessage::CycleFocusWorkspace(direction) => {
                // This is to ensure that even on an empty workspace on a secondary monitor, the
                // secondary monitor where the cursor is focused will be used as the target for
                // the workspace switch op
                if let Some(monitor_idx) = self.monitor_idx_from_current_pos()
                    && monitor_idx != self.focused_monitor_idx()
                    && let Some(monitor) = self.monitors().get(monitor_idx)
                    && let Some(workspace) = monitor.focused_workspace()
                    && workspace.is_empty()
                {
                    self.focus_monitor(monitor_idx)?;
                }

                let focused_monitor = self
                    .focused_monitor()
                    .ok_or_else(|| eyre!("there is no monitor"))?;

                let focused_workspace_idx = focused_monitor.focused_workspace_idx();
                let workspaces = focused_monitor.workspaces().len();

                let workspace_idx = direction.next_idx(
                    focused_workspace_idx,
                    NonZeroUsize::new(workspaces)
                        .ok_or_else(|| eyre!("there must be at least one workspace"))?,
                );

                self.focus_workspace(workspace_idx)?;
            }
            SocketMessage::CycleFocusEmptyWorkspace(direction) => {
                // TODO: figure out if we need to do this on macOS
                // // This is to ensure that even on an empty workspace on a secondary monitor, the
                // // secondary monitor where the cursor is focused will be used as the target for
                // // the workspace switch op
                // if let Some(monitor_idx) = self.monitor_idx_from_current_pos() {
                //     if monitor_idx != self.focused_monitor_idx() {
                //         if let Some(monitor) = self.monitors().get(monitor_idx) {
                //             if let Some(workspace) = monitor.focused_workspace() {
                //                 if workspace.is_empty() {
                //                     self.focus_monitor(monitor_idx)?;
                //                 }
                //             }
                //         }
                //     }
                // }

                let focused_monitor = self
                    .focused_monitor()
                    .ok_or_else(|| eyre!("there is no monitor"))?;

                let focused_workspace_idx = focused_monitor.focused_workspace_idx();
                let workspaces = focused_monitor.workspaces().len();

                let mut empty_workspaces = vec![];

                for (idx, w) in focused_monitor.workspaces().iter().enumerate() {
                    if w.is_empty() {
                        empty_workspaces.push(idx);
                    }
                }

                if !empty_workspaces.is_empty() {
                    let mut workspace_idx = direction.next_idx(
                        focused_workspace_idx,
                        NonZeroUsize::new(workspaces)
                            .ok_or_else(|| eyre!("there must be at least one workspace"))?,
                    );

                    while !empty_workspaces.contains(&workspace_idx) {
                        workspace_idx = direction.next_idx(
                            workspace_idx,
                            NonZeroUsize::new(workspaces)
                                .ok_or_else(|| eyre!("there must be at least one workspace"))?,
                        );
                    }

                    self.focus_workspace(workspace_idx)?;
                }
            }
            SocketMessage::FocusMonitorNumber(monitor_idx) => {
                self.focus_monitor(monitor_idx)?;
                self.update_focused_workspace(self.mouse_follows_focus, true)?;
            }
            SocketMessage::FocusMonitorAtCursor => {
                if let Some(monitor_idx) = self.monitor_idx_from_current_pos() {
                    self.focus_monitor(monitor_idx)?;
                }
            }
            SocketMessage::FocusLastWorkspace => {
                // TODO: figure out if we need to do this on macOS
                // // This is to ensure that even on an empty workspace on a secondary monitor, the
                // // secondary monitor where the cursor is focused will be used as the target for
                // // the workspace switch op
                // if let Some(monitor_idx) = self.monitor_idx_from_current_pos() {
                //     if monitor_idx != self.focused_monitor_idx() {
                //         if let Some(monitor) = self.monitors().get(monitor_idx) {
                //             if let Some(workspace) = monitor.focused_workspace() {
                //                 if workspace.is_empty() {
                //                     self.focus_monitor(monitor_idx)?;
                //                 }
                //             }
                //         }
                //     }
                // }

                let idx = self
                    .focused_monitor()
                    .ok_or_else(|| eyre!("there is no monitor"))?
                    .focused_workspace_idx();

                if let Some(monitor) = self.focused_monitor_mut()
                    && let Some(last_focused_workspace) = monitor.last_focused_workspace
                {
                    self.focus_workspace(last_focused_workspace)?;
                }

                self.focused_monitor_mut()
                    .ok_or_else(|| eyre!("there is no monitor"))?
                    .last_focused_workspace = Option::from(idx);
            }
            SocketMessage::FocusWorkspaceNumber(workspace_idx) => {
                if self.focused_workspace_idx().unwrap_or_default() != workspace_idx {
                    self.focus_workspace(workspace_idx)?;
                }
            }
            SocketMessage::FocusWorkspaceNumbers(workspace_idx) => {
                // TODO: figure out if we need to do this on macOS
                // // This is to ensure that even on an empty workspace on a secondary monitor, the
                // // secondary monitor where the cursor is focused will be used as the target for
                // // the workspace switch op
                // if let Some(monitor_idx) = self.monitor_idx_from_current_pos() {
                //     if monitor_idx != self.focused_monitor_idx() {
                //         if let Some(monitor) = self.monitors().get(monitor_idx) {
                //             if let Some(workspace) = monitor.focused_workspace() {
                //                 if workspace.is_empty() {
                //                     self.focus_monitor(monitor_idx)?;
                //                 }
                //             }
                //         }
                //     }
                // }

                let focused_monitor_idx = self.focused_monitor_idx();

                for (i, monitor) in self.monitors_mut().iter_mut().enumerate() {
                    if i != focused_monitor_idx {
                        monitor.focus_workspace(workspace_idx)?;
                        monitor.load_focused_workspace(false)?;
                    }
                }

                self.focus_workspace(workspace_idx)?;
            }
            SocketMessage::FocusMonitorWorkspaceNumber(monitor_idx, workspace_idx) => {
                let focused_monitor_idx = self.focused_monitor_idx();
                let focused_workspace_idx = self.focused_workspace_idx().unwrap_or_default();

                let focused_pair = (focused_monitor_idx, focused_workspace_idx);

                if focused_pair != (monitor_idx, workspace_idx) {
                    self.focus_monitor(monitor_idx)?;
                    self.focus_workspace(workspace_idx)?;
                }
            }
            SocketMessage::FocusNamedWorkspace(ref name) => {
                if let Some((monitor_idx, workspace_idx)) =
                    self.monitor_workspace_index_by_name(name)
                {
                    self.focus_monitor(monitor_idx)?;
                    self.focus_workspace(workspace_idx)?;
                }
            }
            SocketMessage::CloseWorkspace => {
                // TODO: figure out if we need to do this on macOS
                // // This is to ensure that even on an empty workspace on a secondary monitor, the
                // // secondary monitor where the cursor is focused will be used as the target for
                // // the workspace switch op
                // if let Some(monitor_idx) = self.monitor_idx_from_current_pos() {
                //     if monitor_idx != self.focused_monitor_idx() {
                //         if let Some(monitor) = self.monitors().get(monitor_idx) {
                //             if let Some(workspace) = monitor.focused_workspace() {
                //                 if workspace.is_empty() {
                //                     self.focus_monitor(monitor_idx)?;
                //                 }
                //             }
                //         }
                //     }
                // }

                let mut can_close = false;

                if let Some(monitor) = self.focused_monitor_mut() {
                    let focused_workspace_idx = monitor.focused_workspace_idx();
                    let next_focused_workspace_idx = focused_workspace_idx.saturating_sub(1);

                    if let Some(workspace) = monitor.focused_workspace()
                        && monitor.workspaces().len() > 1
                        && workspace.containers().is_empty()
                        && workspace.floating_windows().is_empty()
                        && workspace.monocle_container.is_none()
                        && workspace.maximized_window.is_none()
                        && workspace.name.is_none()
                    {
                        can_close = true;
                    }

                    if can_close
                        && monitor
                            .workspaces_mut()
                            .remove(focused_workspace_idx)
                            .is_some()
                    {
                        self.focus_workspace(next_focused_workspace_idx)?;
                    }
                }
            }

            SocketMessage::MoveContainerToWorkspaceNumber(workspace_idx) => {
                self.move_container_to_workspace(workspace_idx, true, None)?;
            }
            SocketMessage::SendContainerToWorkspaceNumber(workspace_idx) => {
                self.move_container_to_workspace(workspace_idx, false, None)?;
            }
            SocketMessage::ToggleMonocle => self.toggle_monocle()?,
            SocketMessage::ToggleFloat => self.toggle_float(false)?,
            SocketMessage::ToggleWorkspaceLayer => {
                let mouse_follows_focus = self.mouse_follows_focus;
                let workspace = self.focused_workspace_mut()?;

                let mut to_focus = None;
                match workspace.layer {
                    WorkspaceLayer::Tiling => {
                        workspace.layer = WorkspaceLayer::Floating;
                        tracing::info!("WorkspaceLayer is now Floating");

                        let focused_idx = workspace.focused_floating_window_idx();
                        let mut window_idx_pairs = workspace
                            .floating_windows_mut()
                            .make_contiguous()
                            .iter_mut()
                            .enumerate()
                            .collect::<Vec<_>>();

                        // Sort by window area
                        window_idx_pairs.sort_by_key(|(_, w)| {
                            let rect =
                                Rect::from(MacosApi::window_rect(&w.element).unwrap_or_default());
                            rect.right * rect.bottom
                        });
                        window_idx_pairs.reverse();

                        for (i, window) in window_idx_pairs {
                            if i == focused_idx {
                                to_focus = Some(window.clone());
                            } else {
                                window.restore()?;
                                window.raise()?;
                            }
                        }

                        if let Some(focused_window) = &mut to_focus {
                            // The focused window should be the last one raised to make sure it is
                            // on top
                            focused_window.restore()?;
                            focused_window.raise()?;
                        }

                        for container in workspace.containers() {
                            if let Some(_window) = container.focused_window() {
                                // TODO: figure out z order
                                // window.lower()?;
                            }
                        }

                        if let Some(monocle) = &workspace.monocle_container
                            && let Some(_window) = monocle.focused_window()
                        {
                            // TODO: figure out z order
                            // window.lower()?;
                        }
                    }
                    WorkspaceLayer::Floating => {
                        workspace.layer = WorkspaceLayer::Tiling;
                        tracing::info!("WorkspaceLayer is now Tiling");

                        if let Some(monocle) = &workspace.monocle_container {
                            if let Some(window) = monocle.focused_window() {
                                to_focus = Some(window.clone());
                                window.raise()?;
                            }

                            for window in workspace.floating_windows_mut() {
                                window.hide()?;
                            }
                        } else {
                            let focused_container_idx = workspace.focused_container_idx();
                            for (i, container) in workspace.containers_mut().iter_mut().enumerate()
                            {
                                if let Some(window) = container.focused_window() {
                                    if i == focused_container_idx {
                                        to_focus = Some(window.clone());
                                    }

                                    window.raise()?;
                                }
                            }

                            let mut window_idx_pairs = workspace
                                .floating_windows_mut()
                                .make_contiguous()
                                .iter_mut()
                                .collect::<Vec<_>>();

                            // Sort by window area
                            window_idx_pairs.sort_by_key(|w| {
                                let rect = Rect::from(
                                    MacosApi::window_rect(&w.element).unwrap_or_default(),
                                );
                                rect.right * rect.bottom
                            });

                            for window in window_idx_pairs {
                                // TODO: figure out z order
                                window.hide()?;
                                // window.lower()?;
                            }
                        }
                    }
                };

                if let Some(window) = to_focus {
                    window.focus(mouse_follows_focus)?;
                }
            }
            SocketMessage::ResizeWindowEdge(direction, sizing) => {
                self.resize_window(direction, sizing, self.resize_delta, true)?;
            }
            SocketMessage::ResizeWindowAxis(axis, sizing) => {
                match axis {
                    Axis::Horizontal => {
                        self.resize_window(
                            OperationDirection::Left,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                        self.resize_window(
                            OperationDirection::Right,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                    }
                    Axis::Vertical => {
                        self.resize_window(
                            OperationDirection::Up,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                        self.resize_window(
                            OperationDirection::Down,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                    }
                    Axis::HorizontalAndVertical => {
                        self.resize_window(
                            OperationDirection::Left,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                        self.resize_window(
                            OperationDirection::Right,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                        self.resize_window(
                            OperationDirection::Up,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                        self.resize_window(
                            OperationDirection::Down,
                            sizing,
                            self.resize_delta,
                            false,
                        )?;
                    }
                }

                self.update_focused_workspace(false, false)?;
            }
            SocketMessage::Retile => self.retile_all(false)?,
            SocketMessage::RetileWithResizeDimensions => self.retile_all(true)?,
            SocketMessage::ToggleWorkspaceWindowContainerBehaviour => {
                let current_global_behaviour = self.window_management_behaviour.current_behaviour;
                if let Some(behaviour) =
                    &mut self.focused_workspace_mut()?.window_container_behaviour
                {
                    match behaviour {
                        WindowContainerBehaviour::Create => {
                            *behaviour = WindowContainerBehaviour::Append
                        }
                        WindowContainerBehaviour::Append => {
                            *behaviour = WindowContainerBehaviour::Create
                        }
                    }
                } else {
                    self.focused_workspace_mut()?.window_container_behaviour =
                        Some(match current_global_behaviour {
                            WindowContainerBehaviour::Create => WindowContainerBehaviour::Append,
                            WindowContainerBehaviour::Append => WindowContainerBehaviour::Create,
                        });
                };
            }
            SocketMessage::ToggleWorkspaceFloatOverride => {
                let current_global_override = self.window_management_behaviour.float_override;
                if let Some(float_override) = &mut self.focused_workspace_mut()?.float_override {
                    *float_override = !*float_override;
                } else {
                    self.focused_workspace_mut()?.float_override = Some(!current_global_override);
                };
            }
            SocketMessage::ToggleLock => self.toggle_lock()?,
            SocketMessage::ToggleWindowContainerBehaviour => {
                match self.window_management_behaviour.current_behaviour {
                    WindowContainerBehaviour::Create => {
                        self.window_management_behaviour.current_behaviour =
                            WindowContainerBehaviour::Append;
                    }
                    WindowContainerBehaviour::Append => {
                        self.window_management_behaviour.current_behaviour =
                            WindowContainerBehaviour::Create;
                    }
                }
            }
            SocketMessage::ToggleFloatOverride => {
                self.window_management_behaviour.float_override =
                    !self.window_management_behaviour.float_override;
            }
            SocketMessage::ToggleWindowBasedWorkAreaOffset => {
                let workspace = self.focused_workspace_mut()?;
                workspace.apply_window_based_work_area_offset =
                    !workspace.apply_window_based_work_area_offset;

                self.retile_all(true)?;
            }
            SocketMessage::ToggleCrossMonitorMoveBehaviour => {
                match self.cross_monitor_move_behaviour {
                    MoveBehaviour::Swap => {
                        self.cross_monitor_move_behaviour = MoveBehaviour::Insert;
                    }
                    MoveBehaviour::Insert => {
                        self.cross_monitor_move_behaviour = MoveBehaviour::Swap;
                    }
                    _ => {}
                }
            }
            SocketMessage::ToggleTiling => {
                self.toggle_tiling()?;
            }
            SocketMessage::MoveContainerToLastWorkspace => {
                // This is to ensure that even on an empty workspace on a secondary monitor, the
                // secondary monitor where the cursor is focused will be used as the target for
                // the workspace switch op
                if let Some(monitor_idx) = self.monitor_idx_from_current_pos()
                    && monitor_idx != self.focused_monitor_idx()
                    && let Some(monitor) = self.monitors().get(monitor_idx)
                    && let Some(workspace) = monitor.focused_workspace()
                    && workspace.is_empty()
                {
                    self.focus_monitor(monitor_idx)?;
                }

                let idx = self
                    .focused_monitor()
                    .ok_or_else(|| eyre!("there is no monitor"))?
                    .focused_workspace_idx();

                if let Some(monitor) = self.focused_monitor_mut()
                    && let Some(last_focused_workspace) = monitor.last_focused_workspace
                {
                    self.move_container_to_workspace(last_focused_workspace, true, None)?;
                }

                self.focused_monitor_mut()
                    .ok_or_else(|| eyre!("there is no monitor"))?
                    .last_focused_workspace = Option::from(idx);
            }
            SocketMessage::SendContainerToLastWorkspace => {
                // This is to ensure that even on an empty workspace on a secondary monitor, the
                // secondary monitor where the cursor is focused will be used as the target for
                // the workspace switch op
                if let Some(monitor_idx) = self.monitor_idx_from_current_pos()
                    && monitor_idx != self.focused_monitor_idx()
                    && let Some(monitor) = self.monitors().get(monitor_idx)
                    && let Some(workspace) = monitor.focused_workspace()
                    && workspace.is_empty()
                {
                    self.focus_monitor(monitor_idx)?;
                }

                let idx = self
                    .focused_monitor()
                    .ok_or_else(|| eyre!("there is no monitor"))?
                    .focused_workspace_idx();

                if let Some(monitor) = self.focused_monitor_mut()
                    && let Some(last_focused_workspace) = monitor.last_focused_workspace
                {
                    self.move_container_to_workspace(last_focused_workspace, false, None)?;
                }
                self.focused_monitor_mut()
                    .ok_or_else(|| eyre!("there is no monitor"))?
                    .last_focused_workspace = Option::from(idx);
            }
            SocketMessage::CycleMoveContainerToWorkspace(direction) => {
                let focused_monitor = self
                    .focused_monitor()
                    .ok_or_else(|| eyre!("there is no monitor"))?;

                let focused_workspace_idx = focused_monitor.focused_workspace_idx();
                let workspaces = focused_monitor.workspaces().len();

                let workspace_idx = direction.next_idx(
                    focused_workspace_idx,
                    NonZeroUsize::new(workspaces)
                        .ok_or_else(|| eyre!("there must be at least one workspace"))?,
                );

                self.move_container_to_workspace(workspace_idx, true, None)?;
            }
            SocketMessage::MoveContainerToMonitorNumber(monitor_idx) => {
                let direction = self.direction_from_monitor_idx(monitor_idx);
                self.move_container_to_monitor(monitor_idx, None, true, direction)?;
            }
            SocketMessage::SwapWorkspacesToMonitorNumber(monitor_idx) => {
                self.swap_focused_monitor(monitor_idx)?;
            }
            SocketMessage::CycleMoveContainerToMonitor(direction) => {
                let monitor_idx = direction.next_idx(
                    self.focused_monitor_idx(),
                    NonZeroUsize::new(self.monitors().len())
                        .ok_or_else(|| eyre!("there must be at least one monitor"))?,
                );

                let direction = self.direction_from_monitor_idx(monitor_idx);
                self.move_container_to_monitor(monitor_idx, None, true, direction)?;
            }
            SocketMessage::CycleSendContainerToWorkspace(direction) => {
                let focused_monitor = self
                    .focused_monitor()
                    .ok_or_else(|| eyre!("there is no monitor"))?;

                let focused_workspace_idx = focused_monitor.focused_workspace_idx();
                let workspaces = focused_monitor.workspaces().len();

                let workspace_idx = direction.next_idx(
                    focused_workspace_idx,
                    NonZeroUsize::new(workspaces)
                        .ok_or_else(|| eyre!("there must be at least one workspace"))?,
                );

                self.move_container_to_workspace(workspace_idx, false, None)?;
            }
            SocketMessage::SendContainerToMonitorNumber(monitor_idx) => {
                let direction = self.direction_from_monitor_idx(monitor_idx);
                self.move_container_to_monitor(monitor_idx, None, false, direction)?;
            }
            SocketMessage::CycleSendContainerToMonitor(direction) => {
                let monitor_idx = direction.next_idx(
                    self.focused_monitor_idx(),
                    NonZeroUsize::new(self.monitors().len())
                        .ok_or_else(|| eyre!("there must be at least one monitor"))?,
                );

                let direction = self.direction_from_monitor_idx(monitor_idx);
                self.move_container_to_monitor(monitor_idx, None, false, direction)?;
            }
            SocketMessage::SendContainerToMonitorWorkspaceNumber(monitor_idx, workspace_idx) => {
                let direction = self.direction_from_monitor_idx(monitor_idx);
                self.move_container_to_monitor(
                    monitor_idx,
                    Option::from(workspace_idx),
                    false,
                    direction,
                )?;
            }
            SocketMessage::MoveContainerToMonitorWorkspaceNumber(monitor_idx, workspace_idx) => {
                let direction = self.direction_from_monitor_idx(monitor_idx);
                self.move_container_to_monitor(
                    monitor_idx,
                    Option::from(workspace_idx),
                    true,
                    direction,
                )?;
            }
            SocketMessage::SendContainerToNamedWorkspace(ref workspace) => {
                if let Some((monitor_idx, workspace_idx)) =
                    self.monitor_workspace_index_by_name(workspace)
                {
                    let direction = self.direction_from_monitor_idx(monitor_idx);
                    self.move_container_to_monitor(
                        monitor_idx,
                        Option::from(workspace_idx),
                        false,
                        direction,
                    )?;
                }
            }
            SocketMessage::MoveContainerToNamedWorkspace(ref workspace) => {
                if let Some((monitor_idx, workspace_idx)) =
                    self.monitor_workspace_index_by_name(workspace)
                {
                    let direction = self.direction_from_monitor_idx(monitor_idx);
                    self.move_container_to_monitor(
                        monitor_idx,
                        Option::from(workspace_idx),
                        true,
                        direction,
                    )?;
                }
            }

            SocketMessage::MoveWorkspaceToMonitorNumber(monitor_idx) => {
                self.move_workspace_to_monitor(monitor_idx)?;
            }
            SocketMessage::CycleMoveWorkspaceToMonitor(direction) => {
                let monitor_idx = direction.next_idx(
                    self.focused_monitor_idx(),
                    NonZeroUsize::new(self.monitors().len())
                        .ok_or_else(|| eyre!("there must be at least one monitor"))?,
                );

                self.move_workspace_to_monitor(monitor_idx)?;
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn move_workspace_to_monitor(&mut self, idx: usize) -> eyre::Result<()> {
        tracing::info!("moving workspace");
        let mouse_follows_focus = self.mouse_follows_focus;
        let offset = self.work_area_offset;
        let workspace = self
            .remove_focused_workspace()
            .ok_or_else(|| eyre!("there is no workspace"))?;

        {
            let target_monitor: &mut Monitor = self
                .monitors_mut()
                .get_mut(idx)
                .ok_or_else(|| eyre!("there is no monitor"))?;

            target_monitor.workspaces_mut().push_back(workspace);
            target_monitor.update_workspaces_globals(offset);
            target_monitor.focus_workspace(target_monitor.workspaces().len().saturating_sub(1))?;
            target_monitor.load_focused_workspace(mouse_follows_focus)?;
        }

        self.focus_monitor(idx)?;
        self.update_focused_workspace(mouse_follows_focus, true)
    }

    pub fn remove_focused_workspace(&mut self) -> Option<Workspace> {
        let focused_monitor: &mut Monitor = self.focused_monitor_mut()?;
        let focused_workspace_idx = focused_monitor.focused_workspace_idx();
        let workspace = focused_monitor.remove_workspace_by_idx(focused_workspace_idx);
        if let Err(error) = focused_monitor.focus_workspace(focused_workspace_idx.saturating_sub(1))
        {
            tracing::error!(
                "Error focusing previous workspace while removing the focused workspace: {}",
                error
            );
        }
        workspace
    }
}

pub fn read_commands_uds(
    wm: &Arc<Mutex<WindowManager>>,
    mut stream: UnixStream,
) -> eyre::Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    // TODO(raggi): while this processes more than one command, if there are
    // replies there is no clearly defined protocol for framing yet - it's
    // perhaps whole-json objects for now, but termination is signalled by
    // socket shutdown.
    for line in reader.lines() {
        let message = SocketMessage::from_str(&line?)?;

        match wm.try_lock_for(Duration::from_secs(1)) {
            None => {
                tracing::warn!(
                    "could not acquire window manager lock, not processing message: {message}"
                );
            }
            Some(mut wm) => {
                if wm.is_paused {
                    return match message {
                        SocketMessage::TogglePause
                        // | SocketMessage::State
                        // | SocketMessage::GlobalState
                        // | SocketMessage::Stop
                        => Ok(wm.process_command(message, &mut stream)?),
                        _ => {
                            tracing::trace!("ignoring while paused");
                            Ok(())
                        }
                    };
                }

                wm.process_command(message.clone(), &mut stream)?;
            }
        }
    }

    Ok(())
}

use crate::core::SocketMessage;
use crate::core::rect::Rect;
use crate::macos_api::MacosApi;
use crate::window_manager::WindowManager;
use crate::workspace::WorkspaceLayer;
use color_eyre::eyre;
use parking_lot::Mutex;
use std::io::BufRead;
use std::io::BufReader;
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
            SocketMessage::ChangeLayout(layout) => self.change_workspace_layout_default(layout)?,
            SocketMessage::TogglePause => {
                if self.is_paused {
                    tracing::info!("resuming");
                } else {
                    tracing::info!("pausing");
                }

                self.is_paused = !self.is_paused;
            }
            SocketMessage::FocusWorkspaceNumber(workspace_idx) => {
                if self.focused_workspace_idx().unwrap_or_default() != workspace_idx {
                    self.focus_workspace(workspace_idx)?;
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
                                // todo: figure out z order
                                // window.lower()?;
                            }
                        }

                        if let Some(monocle) = &workspace.monocle_container
                            && let Some(_window) = monocle.focused_window()
                        {
                            // todo: figure out z order
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
                                // todo: figure out z order
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
        }

        Ok(())
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

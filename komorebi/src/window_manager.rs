use crate::CoreFoundationRunLoop;
use crate::DATA_DIR;
use crate::LibraryError;
use crate::REGEX_IDENTIFIERS;
use crate::SUBSCRIPTION_SOCKETS;
use crate::UNMANAGED_WINDOW_IDS;
use crate::WORKSPACE_MATCHING_RULES;
use crate::accessibility::AccessibilityApi;
use crate::application::Application;
use crate::container::Container;
use crate::core::CrossBoundaryBehaviour;
use crate::core::MoveBehaviour;
use crate::core::OperationBehaviour;
use crate::core::Placement;
use crate::core::Sizing;
use crate::core::WindowContainerBehaviour;
use crate::core::WindowManagementBehaviour;
use crate::core::arrangement::Arrangement;
use crate::core::arrangement::Axis;
use crate::core::config_generation::MatchingRule;
use crate::core::cycle_direction::CycleDirection;
use crate::core::default_layout::DefaultLayout;
use crate::core::layout::Layout;
use crate::core::operation_direction::OperationDirection;
use crate::core::rect::Rect;
use crate::current_space_id;
use crate::lockable_sequence::Lockable;
use crate::macos_api::MacosApi;
use crate::monitor::Monitor;
use crate::ring::Ring;
use crate::static_config::StaticConfig;
use crate::window::AdhocWindow;
use crate::window::Window;
use crate::window::should_act_individual;
use crate::window_manager_event::ManualNotification;
use crate::window_manager_event::SystemNotification;
use crate::window_manager_event::WindowManagerEvent;
use crate::window_manager_event_listener;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceLayer;
use color_eyre::eyre;
use color_eyre::eyre::OptionExt;
use color_eyre::eyre::bail;
use crossbeam_channel::Receiver;
use hotwatch::Hotwatch;
use objc2_core_foundation::CFRetained;
use objc2_core_foundation::CFRunLoop;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::io::ErrorKind;
use std::net::Shutdown;
use std::num::NonZeroUsize;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug)]
pub struct WindowManager {
    pub monitors: Ring<Monitor>,
    pub monitor_usr_idx_map: HashMap<usize, usize>,
    pub applications: HashMap<i32, Application>,
    pub run_loop: CoreFoundationRunLoop,
    pub command_listener: UnixListener,
    pub space_id: Option<u64>,
    pub is_paused: bool,
    pub resize_delta: i32,
    pub hotwatch: Hotwatch,
    pub unmanaged_window_operation_behaviour: OperationBehaviour,
    pub window_management_behaviour: WindowManagementBehaviour,
    pub cross_monitor_move_behaviour: MoveBehaviour,
    pub cross_boundary_behaviour: CrossBoundaryBehaviour,
    pub mouse_follows_focus: bool,
    pub work_area_offset: Option<Rect>,
    pub incoming_events: Receiver<WindowManagerEvent>,
    pub minimized_windows: HashMap<u32, Window>,
    pub pending_move_op: Arc<Option<(usize, usize, u32)>>,
    pub pending_resize_op: Arc<Option<(u32, Option<Rect>)>>,
    pub already_moved_window_handles: Arc<Mutex<HashSet<u32>>>,
    /// Maps each known window id to the (monitor, workspace) index pair managing it
    pub known_window_ids: HashMap<u32, (usize, usize)>,
}

impl_ring_elements!(WindowManager, Monitor);

#[derive(Debug, Clone, Copy)]
struct EnforceWorkspaceRuleOp {
    window_id: u32,
    origin_monitor_idx: usize,
    origin_workspace_idx: usize,
    target_monitor_idx: usize,
    target_workspace_idx: usize,
    floating: bool,
}

impl EnforceWorkspaceRuleOp {
    const fn is_origin(&self, monitor_idx: usize, workspace_idx: usize) -> bool {
        self.origin_monitor_idx == monitor_idx && self.origin_workspace_idx == workspace_idx
    }

    const fn is_target(&self, monitor_idx: usize, workspace_idx: usize) -> bool {
        self.target_monitor_idx == monitor_idx && self.target_workspace_idx == workspace_idx
    }

    const fn is_enforced(&self) -> bool {
        (self.origin_monitor_idx == self.target_monitor_idx)
            && (self.origin_workspace_idx == self.target_workspace_idx)
    }
}

impl AsRef<Self> for WindowManager {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl WindowManager {
    #[allow(clippy::field_reassign_with_default)]
    pub fn new(
        run_loop: &CFRetained<CFRunLoop>,
        incoming: Receiver<WindowManagerEvent>,
        custom_socket_path: Option<PathBuf>,
    ) -> eyre::Result<Self> {
        let socket = custom_socket_path.unwrap_or_else(|| DATA_DIR.join("komorebi.sock"));

        match std::fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) => match error.kind() {
                ErrorKind::NotFound => {}
                _ => {
                    return Err(error.into());
                }
            },
        };

        let listener = UnixListener::bind(&socket)?;

        // TODO: undo this when we get config
        let mut behaviour = WindowManagementBehaviour::default();
        behaviour.toggle_float_placement = Placement::CenterAndResize;

        Ok(Self {
            monitors: Ring::default(),
            monitor_usr_idx_map: HashMap::new(),
            applications: Default::default(),
            run_loop: CoreFoundationRunLoop(run_loop.clone()),
            command_listener: listener,
            space_id: current_space_id(),
            is_paused: false,
            resize_delta: 50,
            hotwatch: Hotwatch::new()?,
            unmanaged_window_operation_behaviour: Default::default(),
            window_management_behaviour: behaviour,
            cross_monitor_move_behaviour: Default::default(),
            cross_boundary_behaviour: Default::default(),
            mouse_follows_focus: true,
            work_area_offset: None,
            incoming_events: incoming,
            minimized_windows: HashMap::new(),
            pending_move_op: Arc::new(None),
            pending_resize_op: Arc::new(None),
            already_moved_window_handles: Default::default(),
            known_window_ids: Default::default(),
        })
    }

    #[tracing::instrument(skip(self))]
    pub fn init(&mut self) -> Result<(), LibraryError> {
        tracing::info!("initializing");
        MacosApi::load_monitor_information(self)?;
        MacosApi::load_workspace_information(self)
    }

    pub fn application(&mut self, process_id: i32) -> Result<&mut Application, LibraryError> {
        match self.applications.entry(process_id) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(vacant) => {
                let mut application = Application::new(process_id)?;
                application.observe(&self.run_loop);
                Ok(vacant.insert(application))
            }
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_floating_window_in_direction(
        &mut self,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        let mouse_follows_focus = self.mouse_follows_focus;
        let focused_workspace = self.focused_workspace_mut()?;

        let mut target_idx = None;
        let len = focused_workspace.floating_windows().len();

        if len > 1 {
            let focused_window_id =
                MacosApi::foreground_window_id().ok_or_eyre("no foreground window")?;
            let focused_rect = Rect::from(MacosApi::window_rect(
                MacosApi::foreground_window()
                    .ok_or_eyre("no foreground window")?
                    .as_ref(),
            )?);

            match direction {
                OperationDirection::Left => {
                    let mut windows_in_direction = focused_workspace
                        .floating_windows()
                        .iter()
                        .enumerate()
                        .flat_map(|(idx, w)| {
                            (w.id != focused_window_id).then_some(
                                MacosApi::window_rect(&w.element)
                                    .ok()
                                    .map(|r| (idx, Rect::from(r))),
                            )
                        })
                        .flatten()
                        .flat_map(|(idx, r)| {
                            (r.left < focused_rect.left)
                                .then_some((idx, i32::abs(r.left - focused_rect.left)))
                        })
                        .collect::<Vec<_>>();

                    // Sort by distance to focused
                    windows_in_direction.sort_by_key(|(_, d)| (*d as f32 * 1000.0).trunc() as i32);

                    if let Some((idx, _)) = windows_in_direction.first() {
                        target_idx = Some(*idx);
                    }
                }
                OperationDirection::Right => {
                    let mut windows_in_direction = focused_workspace
                        .floating_windows()
                        .iter()
                        .enumerate()
                        .flat_map(|(idx, w)| {
                            (w.id != focused_window_id).then_some(
                                MacosApi::window_rect(&w.element)
                                    .ok()
                                    .map(|r| (idx, Rect::from(r))),
                            )
                        })
                        .flatten()
                        .flat_map(|(idx, r)| {
                            (r.left > focused_rect.left)
                                .then_some((idx, i32::abs(r.left - focused_rect.left)))
                        })
                        .collect::<Vec<_>>();

                    // Sort by distance to focused
                    windows_in_direction.sort_by_key(|(_, d)| (*d as f32 * 1000.0).trunc() as i32);

                    if let Some((idx, _)) = windows_in_direction.first() {
                        target_idx = Some(*idx);
                    }
                }
                OperationDirection::Up => {
                    let mut windows_in_direction = focused_workspace
                        .floating_windows()
                        .iter()
                        .enumerate()
                        .flat_map(|(idx, w)| {
                            (w.id != focused_window_id).then_some(
                                MacosApi::window_rect(&w.element)
                                    .ok()
                                    .map(|r| (idx, Rect::from(r))),
                            )
                        })
                        .flatten()
                        .flat_map(|(idx, r)| {
                            (r.top < focused_rect.top)
                                .then_some((idx, i32::abs(r.top - focused_rect.top)))
                        })
                        .collect::<Vec<_>>();

                    // Sort by distance to focused
                    windows_in_direction.sort_by_key(|(_, d)| (*d as f32 * 1000.0).trunc() as i32);

                    if let Some((idx, _)) = windows_in_direction.first() {
                        target_idx = Some(*idx);
                    }
                }
                OperationDirection::Down => {
                    let mut windows_in_direction = focused_workspace
                        .floating_windows()
                        .iter()
                        .enumerate()
                        .flat_map(|(idx, w)| {
                            (w.id != focused_window_id).then_some(
                                MacosApi::window_rect(&w.element)
                                    .ok()
                                    .map(|r| (idx, Rect::from(r))),
                            )
                        })
                        .flatten()
                        .flat_map(|(idx, r)| {
                            (r.top > focused_rect.top)
                                .then_some((idx, i32::abs(r.top - focused_rect.top)))
                        })
                        .collect::<Vec<_>>();

                    // Sort by distance to focused
                    windows_in_direction.sort_by_key(|(_, d)| (*d as f32 * 1000.0).trunc() as i32);

                    if let Some((idx, _)) = windows_in_direction.first() {
                        target_idx = Some(*idx);
                    }
                }
            };
        }

        if let Some(idx) = target_idx {
            focused_workspace.floating_windows.focus(idx);
            if let Some(window) = focused_workspace.floating_windows().get(idx) {
                window.focus(mouse_follows_focus)?;
            }
            return Ok(());
        }

        let mut cross_monitor_monocle_or_max = false;

        let workspace_idx = self.focused_workspace_idx()?;

        // this is for when we are scrolling across workspaces like PaperWM
        if matches!(
            self.cross_boundary_behaviour,
            CrossBoundaryBehaviour::Workspace
        ) && matches!(
            direction,
            OperationDirection::Left | OperationDirection::Right
        ) {
            let workspace_count = if let Some(monitor) = self.focused_monitor() {
                monitor.workspaces().len()
            } else {
                1
            };

            let next_idx = match direction {
                OperationDirection::Left => match workspace_idx {
                    0 => workspace_count - 1,
                    n => n - 1,
                },
                OperationDirection::Right => match workspace_idx {
                    n if n == workspace_count - 1 => 0,
                    n => n + 1,
                },
                _ => workspace_idx,
            };

            self.focus_workspace(next_idx)?;

            if let Ok(focused_workspace) = self.focused_workspace_mut()
                && focused_workspace.monocle_container.is_none()
            {
                match direction {
                    OperationDirection::Left => match focused_workspace.layout {
                        Layout::Default(layout) => {
                            let target_index =
                                layout.rightmost_index(focused_workspace.containers().len());
                            focused_workspace.focus_container(target_index);
                        }
                    },
                    OperationDirection::Right => match focused_workspace.layout {
                        Layout::Default(layout) => {
                            let target_index =
                                layout.leftmost_index(focused_workspace.containers().len());
                            focused_workspace.focus_container(target_index);
                        }
                    },
                    _ => {}
                };
            }

            return Ok(());
        }

        // if there is no floating_window in that direction for this workspace
        let monitor_idx = self
            .monitor_idx_in_direction(direction)
            .ok_or_eyre("there is no container or monitor in this direction")?;

        self.focus_monitor(monitor_idx)?;
        let mouse_follows_focus = self.mouse_follows_focus;

        if let Ok(focused_workspace) = self.focused_workspace_mut() {
            // if let Some(window) = focused_workspace.maximized_window() {
            //     window.focus(mouse_follows_focus)?;
            //     cross_monitor_monocle_or_max = true;
            // } else
            if let Some(monocle) = &focused_workspace.monocle_container {
                if let Some(window) = monocle.focused_window() {
                    window.focus(mouse_follows_focus)?;
                    cross_monitor_monocle_or_max = true;
                }
            } else if focused_workspace.layer == WorkspaceLayer::Tiling {
                match direction {
                    OperationDirection::Left => match focused_workspace.layout {
                        Layout::Default(layout) => {
                            let target_index =
                                layout.rightmost_index(focused_workspace.containers().len());
                            focused_workspace.focus_container(target_index);
                        }
                    },
                    OperationDirection::Right => match focused_workspace.layout {
                        Layout::Default(layout) => {
                            let target_index =
                                layout.leftmost_index(focused_workspace.containers().len());
                            focused_workspace.focus_container(target_index);
                        }
                    },
                    _ => {}
                };
            }
        }

        if !cross_monitor_monocle_or_max {
            let ws = self.focused_workspace_mut()?;
            if ws.is_empty() {
                // TODO: figure out if we need to do this on macos
                // This is to remove focus from the previous monitor
                // let desktop_window = Window::from(MacosApi::desktop_window()?);
                //
                // match MacosApi::raise_and_focus_window(desktop_window.id) {
                //     Ok(()) => {}
                //     Err(error) => {
                //         tracing::warn!("{} {}:{}", error, file!(), line!());
                //     }
                // }
            } else if ws.layer == WorkspaceLayer::Floating && !ws.floating_windows().is_empty() {
                if let Some(window) = ws.focused_floating_window() {
                    window.focus(mouse_follows_focus)?;
                }
            } else {
                ws.layer = WorkspaceLayer::Tiling;
                if let Ok(focused_window) = self.focused_window() {
                    focused_window.focus(mouse_follows_focus)?;
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_container_in_direction(
        &mut self,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        self.handle_unmanaged_window_behaviour()?;
        let mouse_follows_focus = self.mouse_follows_focus;

        let workspace = self.focused_workspace()?;
        let workspace_idx = self.focused_workspace_idx()?;

        tracing::info!("focusing container");

        let new_idx =
            if workspace.maximized_window.is_some() || workspace.monocle_container.is_some() {
                None
            } else {
                workspace.new_idx_for_direction(direction)
            };

        let mut cross_monitor_monocle_or_max = false;

        // this is for when we are scrolling across workspaces like PaperWM
        if new_idx.is_none()
            && matches!(
                self.cross_boundary_behaviour,
                CrossBoundaryBehaviour::Workspace
            )
            && matches!(
                direction,
                OperationDirection::Left | OperationDirection::Right
            )
        {
            let workspace_count = if let Some(monitor) = self.focused_monitor() {
                monitor.workspaces().len()
            } else {
                1
            };

            let next_idx = match direction {
                OperationDirection::Left => match workspace_idx {
                    0 => workspace_count - 1,
                    n => n - 1,
                },
                OperationDirection::Right => match workspace_idx {
                    n if n == workspace_count - 1 => 0,
                    n => n + 1,
                },
                _ => workspace_idx,
            };

            self.focus_workspace(next_idx)?;

            if let Ok(focused_workspace) = self.focused_workspace_mut()
                && focused_workspace.monocle_container.is_none()
            {
                match direction {
                    OperationDirection::Left => match focused_workspace.layout {
                        Layout::Default(layout) => {
                            let target_index =
                                layout.rightmost_index(focused_workspace.containers().len());
                            focused_workspace.focus_container(target_index);
                        }
                    },
                    OperationDirection::Right => match focused_workspace.layout {
                        Layout::Default(layout) => {
                            let target_index =
                                layout.leftmost_index(focused_workspace.containers().len());
                            focused_workspace.focus_container(target_index);
                        }
                    },
                    _ => {}
                };
            }

            return Ok(());
        }

        // if there is no container in that direction for this workspace
        match new_idx {
            None => {
                let monitor_idx = self
                    .monitor_idx_in_direction(direction)
                    .ok_or_eyre("there is no container or monitor in this direction")?;

                self.focus_monitor(monitor_idx)?;

                if let Ok(focused_workspace) = self.focused_workspace_mut() {
                    if let Some(window) = &focused_workspace.maximized_window {
                        window.focus(mouse_follows_focus)?;
                        cross_monitor_monocle_or_max = true;
                    } else if let Some(monocle) = &focused_workspace.monocle_container {
                        if let Some(window) = monocle.focused_window() {
                            window.focus(mouse_follows_focus)?;
                            cross_monitor_monocle_or_max = true;
                        }
                    } else if focused_workspace.layer == WorkspaceLayer::Tiling {
                        match direction {
                            OperationDirection::Left => match focused_workspace.layout {
                                Layout::Default(layout) => {
                                    let target_index = layout
                                        .rightmost_index(focused_workspace.containers().len());
                                    focused_workspace.focus_container(target_index);
                                }
                            },
                            OperationDirection::Right => match focused_workspace.layout {
                                Layout::Default(layout) => {
                                    let target_index =
                                        layout.leftmost_index(focused_workspace.containers().len());
                                    focused_workspace.focus_container(target_index);
                                }
                            },
                            _ => {}
                        };
                    }
                }
            }
            Some(idx) => {
                let workspace = self.focused_workspace_mut()?;
                workspace.focus_container(idx);
            }
        }

        if !cross_monitor_monocle_or_max {
            let ws = self.focused_workspace_mut()?;
            if ws.is_empty() {
                // TODO: figure out if we need to do this on macOS
                // This is to remove focus from the previous monitor
                // let desktop_window = Window::from(WindowsApi::desktop_window()?);
                //
                // match WindowsApi::raise_and_focus_window(desktop_window.hwnd) {
                //     Ok(()) => {}
                //     Err(error) => {
                //         tracing::warn!("{} {}:{}", error, file!(), line!());
                //     }
                // }
            } else if ws.layer == WorkspaceLayer::Floating && !ws.floating_windows().is_empty() {
                if let Some(window) = ws.focused_floating_window() {
                    window.focus(mouse_follows_focus)?;
                }
            } else {
                ws.layer = WorkspaceLayer::Tiling;
                if let Ok(focused_window) = self.focused_window() {
                    focused_window.focus(mouse_follows_focus)?;
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn move_floating_window_in_direction(
        &mut self,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        let mouse_follows_focus = self.mouse_follows_focus;

        let mut focused_monitor_work_area = self.focused_monitor_work_area()?;
        let border_offset = 0;
        let border_width = 0;
        focused_monitor_work_area.left += border_offset;
        focused_monitor_work_area.left += border_width;
        focused_monitor_work_area.top += border_offset;
        focused_monitor_work_area.top += border_width;
        focused_monitor_work_area.right -= border_offset * 2;
        focused_monitor_work_area.right -= border_width * 2;
        focused_monitor_work_area.bottom -= border_offset * 2;
        focused_monitor_work_area.bottom -= border_width * 2;

        let focused_workspace = self.focused_workspace()?;
        let delta = self.resize_delta;

        let focused_window_id =
            MacosApi::foreground_window_id().ok_or_eyre("no foreground window")?;
        for window in focused_workspace.floating_windows().iter() {
            if window.id == focused_window_id {
                let mut rect = Rect::from(MacosApi::window_rect(&window.element)?);
                match direction {
                    OperationDirection::Left => {
                        if rect.left - delta < focused_monitor_work_area.left {
                            rect.left = focused_monitor_work_area.left;
                        } else {
                            rect.left -= delta;
                        }
                    }
                    OperationDirection::Right => {
                        if rect.left + delta + rect.right
                            > focused_monitor_work_area.left + focused_monitor_work_area.right
                        {
                            rect.left = focused_monitor_work_area.left
                                + focused_monitor_work_area.right
                                - rect.right;
                        } else {
                            rect.left += delta;
                        }
                    }
                    OperationDirection::Up => {
                        if rect.top - delta < focused_monitor_work_area.top {
                            rect.top = focused_monitor_work_area.top;
                        } else {
                            rect.top -= delta;
                        }
                    }
                    OperationDirection::Down => {
                        if rect.top + delta + rect.bottom
                            > focused_monitor_work_area.top + focused_monitor_work_area.bottom
                        {
                            rect.top = focused_monitor_work_area.top
                                + focused_monitor_work_area.bottom
                                - rect.bottom;
                        } else {
                            rect.top += delta;
                        }
                    }
                }

                window.set_position(&rect)?;

                if mouse_follows_focus {
                    MacosApi::center_cursor_in_rect(&rect)?;
                }

                break;
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_in_direction(
        &mut self,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        self.handle_unmanaged_window_behaviour()?;

        let workspace = self.focused_workspace()?;
        let workspace_idx = self.focused_workspace_idx()?;

        // removing this messes up the monitor / container / window index somewhere
        // and results in the wrong window getting moved across the monitor boundary
        if workspace.is_focused_window_monocle_or_maximized()? {
            bail!("ignoring command while active window is in monocle mode or maximized");
        }

        tracing::info!("moving container");

        let origin_container_idx = workspace.focused_container_idx();
        let origin_monitor_idx = self.focused_monitor_idx();
        let target_container_idx = workspace.new_idx_for_direction(direction);

        // this is for when we are scrolling across workspaces like PaperWM
        if target_container_idx.is_none()
            && matches!(
                self.cross_boundary_behaviour,
                CrossBoundaryBehaviour::Workspace
            )
            && matches!(
                direction,
                OperationDirection::Left | OperationDirection::Right
            )
        {
            let workspace_count = if let Some(monitor) = self.focused_monitor() {
                monitor.workspaces().len()
            } else {
                1
            };

            let next_idx = match direction {
                OperationDirection::Left => match workspace_idx {
                    0 => workspace_count - 1,
                    n => n - 1,
                },
                OperationDirection::Right => match workspace_idx {
                    n if n == workspace_count - 1 => 0,
                    n => n + 1,
                },
                _ => workspace_idx,
            };

            // passing the direction here is how we handle whether to insert at the front
            // or the back of the container vecdeque in the target workspace
            self.move_container_to_workspace(next_idx, true, Some(direction))?;
            self.update_focused_workspace(self.mouse_follows_focus, true)?;

            return Ok(());
        }

        match target_container_idx {
            // If there is nowhere to move on the current workspace, try to move it onto the monitor
            // in that direction if there is one
            None => {
                // Don't do anything if the user has set the MoveBehaviour to NoOp
                if matches!(self.cross_monitor_move_behaviour, MoveBehaviour::NoOp) {
                    return Ok(());
                }

                let target_monitor_idx = self
                    .monitor_idx_in_direction(direction)
                    .ok_or_eyre("there is no container or monitor in this direction")?;

                {
                    // actually move the container to target monitor using the direction
                    self.move_container_to_monitor(
                        target_monitor_idx,
                        None,
                        true,
                        Some(direction),
                    )?;

                    // focus the target monitor
                    self.focus_monitor(target_monitor_idx)?;

                    // unset monocle container on target workspace if there is one
                    let mut target_workspace_has_monocle = false;
                    if let Ok(target_workspace) = self.focused_workspace()
                        && target_workspace.monocle_container.is_some()
                    {
                        target_workspace_has_monocle = true;
                    }

                    if target_workspace_has_monocle {
                        self.toggle_monocle()?;
                    }

                    // get a mutable ref to the focused workspace on the target monitor
                    let target_workspace = self.focused_workspace_mut()?;

                    // if there is only one container on the target workspace after the insertion
                    // it means that there won't be one swapped back, so we have to decrement the
                    // focused position
                    if target_workspace.containers().len() == 1 {
                        let origin_workspace =
                            self.focused_workspace_for_monitor_idx_mut(origin_monitor_idx)?;

                        origin_workspace.focus_container(
                            origin_workspace.focused_container_idx().saturating_sub(1),
                        );
                    }
                }

                // if our MoveBehaviour is Swap, let's try to send back the window container
                // whose position which just took over
                if matches!(self.cross_monitor_move_behaviour, MoveBehaviour::Swap) {
                    {
                        let target_workspace = self.focused_workspace_mut()?;

                        // if the target workspace doesn't have more than one container, this means it
                        // was previously empty, by only doing the second part of the swap when there is
                        // more than one container, we can fall back to a "move" if there is nothing to
                        // swap with on the target monitor
                        if target_workspace.containers().len() > 1 {
                            // remove the container from the target monitor workspace
                            let target_container = target_workspace
                                // this is now focused_container_idx + 1 because we have inserted our origin container
                                .remove_container_by_idx(
                                    target_workspace.focused_container_idx() + 1,
                                )
                                .ok_or_eyre("could not remove container at given target index")?;

                            let origin_workspace =
                                self.focused_workspace_for_monitor_idx_mut(origin_monitor_idx)?;

                            // insert the container from the target monitor workspace into the origin monitor workspace
                            // at the same position from which our origin container was removed
                            origin_workspace
                                .insert_container_at_idx(origin_container_idx, target_container);
                        }
                    }
                }

                // make sure to update the origin monitor workspace layout because it is no
                // longer focused so it won't get updated at the end of this fn
                let offset = self.work_area_offset;

                self.monitors_mut()
                    .get_mut(origin_monitor_idx)
                    .ok_or_eyre("there is no monitor at this index")?
                    .update_focused_workspace(offset)?;

                // TODO: figure out monitor DPI differences on macOS
                // let a = self
                //     .focused_monitor()
                //     .ok_or_eyre("there is no monitor focused monitor")?
                //     .id;
                // let b = self
                //     .monitors_mut()
                //     .get_mut(origin_monitor_idx)
                //     .ok_or_eyre("there is no monitor at this index")?
                //     .id;

                // if !WindowsApi::monitors_have_same_dpi(a, b)? {
                //     self.update_focused_workspace(self.mouse_follows_focus, true)?;
                // }
            }
            Some(new_idx) => {
                let workspace = self.focused_workspace_mut()?;
                workspace.swap_containers(origin_container_idx, new_idx);
                workspace.focus_container(new_idx);
            }
        }

        self.update_focused_workspace(self.mouse_follows_focus, true)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn update_focused_workspace(
        &mut self,
        follow_focus: bool,
        trigger_focus: bool,
    ) -> eyre::Result<()> {
        let offset = self.work_area_offset;
        let mouse_follows_focus = self.mouse_follows_focus;

        self.focused_monitor_mut()
            .ok_or_eyre("there is no monitor")?
            .update_focused_workspace(offset)?;

        if follow_focus
            && let Ok(window) = self.focused_window_mut()
            && trigger_focus
        {
            window.focus(mouse_follows_focus)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn change_workspace_layout_default(&mut self, layout: DefaultLayout) -> eyre::Result<()> {
        tracing::info!("changing layout");

        let monitor_count = self.monitors().len();
        let workspace = self.focused_workspace_mut()?;

        if monitor_count > 1 && matches!(layout, DefaultLayout::Scrolling) {
            tracing::warn!(
                "scrolling layout is only supported for a single monitor; not changing layout"
            );
            return Ok(());
        }

        workspace.layout = Layout::Default(layout);
        self.update_focused_workspace(self.mouse_follows_focus, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn set_workspace_layout_default(
        &mut self,
        monitor_idx: usize,
        workspace_idx: usize,
        layout: DefaultLayout,
    ) -> eyre::Result<()> {
        tracing::info!("setting workspace layout");

        let focused_monitor_idx = self.focused_monitor_idx();

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let focused_workspace_idx = monitor.focused_workspace_idx();

        let workspace = monitor
            .workspaces_mut()
            .get_mut(workspace_idx)
            .ok_or_eyre("there is no monitor")?;

        workspace.layout = Layout::Default(layout);

        // If this is the focused workspace on a non-focused screen, let's update it
        if focused_monitor_idx != monitor_idx && focused_workspace_idx == workspace_idx {
            workspace.update()?;
            Ok(())
        } else {
            Ok(self.update_focused_workspace(false, false)?)
        }
    }

    pub fn focused_workspace_idx(&self) -> eyre::Result<usize> {
        Ok(self
            .focused_monitor()
            .ok_or_eyre("there is no monitor")?
            .focused_workspace_idx())
    }

    pub fn focused_workspace(&self) -> eyre::Result<&Workspace> {
        self.focused_monitor()
            .ok_or_eyre("there is no monitor")?
            .focused_workspace()
            .ok_or_eyre("there is no workspace")
    }

    pub fn focused_workspace_mut(&mut self) -> eyre::Result<&mut Workspace> {
        self.focused_monitor_mut()
            .ok_or_eyre("there is no monitor")?
            .focused_workspace_mut()
            .ok_or_eyre("there is no workspace")
    }

    pub fn focused_workspace_idx_for_monitor_idx(&self, idx: usize) -> eyre::Result<usize> {
        Ok(self
            .monitors()
            .get(idx)
            .ok_or_eyre("there is no monitor at this index")?
            .focused_workspace_idx())
    }

    pub fn focused_workspace_for_monitor_idx(&self, idx: usize) -> eyre::Result<&Workspace> {
        self.monitors()
            .get(idx)
            .ok_or_eyre("there is no monitor at this index")?
            .focused_workspace()
            .ok_or_eyre("there is no workspace")
    }

    pub fn focused_workspace_for_monitor_idx_mut(
        &mut self,
        idx: usize,
    ) -> eyre::Result<&mut Workspace> {
        self.monitors_mut()
            .get_mut(idx)
            .ok_or_eyre("there is no monitor at this index")?
            .focused_workspace_mut()
            .ok_or_eyre("there is no workspace")
    }

    #[tracing::instrument(skip(self))]
    pub fn new_workspace(&mut self) -> eyre::Result<()> {
        tracing::info!("adding new workspace");

        let mouse_follows_focus = self.mouse_follows_focus;
        let monitor = self
            .focused_monitor_mut()
            .ok_or_eyre("there is no workspace")?;

        monitor.focus_workspace(monitor.new_workspace_idx())?;
        monitor.load_focused_workspace(mouse_follows_focus)?;

        self.update_focused_workspace(self.mouse_follows_focus, false)
    }

    pub fn focused_container(&self) -> eyre::Result<&Container> {
        self.focused_workspace()?
            .focused_container()
            .ok_or_eyre("there is no container")
    }

    pub fn focused_container_idx(&self) -> eyre::Result<usize> {
        Ok(self.focused_workspace()?.focused_container_idx())
    }

    pub fn focused_container_mut(&mut self) -> eyre::Result<&mut Container> {
        self.focused_workspace_mut()?
            .focused_container_mut()
            .ok_or_eyre("there is no container")
    }

    pub fn focused_window(&self) -> eyre::Result<&Window> {
        self.focused_container()?
            .focused_window()
            .ok_or_eyre("there is no window")
    }

    fn focused_window_mut(&mut self) -> eyre::Result<&mut Window> {
        self.focused_container_mut()?
            .focused_window_mut()
            .ok_or_eyre("there is no window")
    }

    pub fn focused_monitor_size(&self) -> eyre::Result<Rect> {
        Ok(self
            .focused_monitor()
            .ok_or_eyre("there is no monitor")?
            .size)
    }

    pub fn focused_monitor_work_area(&self) -> eyre::Result<Rect> {
        Ok(self
            .focused_monitor()
            .ok_or_eyre("there is no monitor")?
            .work_area_size)
    }

    pub fn extract_minimized_window(&mut self, window_id: u32) -> eyre::Result<()> {
        let workspace = self.focused_workspace_mut()?;
        let window = workspace.remove_window(window_id)?;
        self.minimized_windows.insert(window_id, window);

        Ok(())
    }

    pub fn reap_invalid_windows_for_application(&mut self, process_id: i32) -> eyre::Result<()> {
        let application = self.application(process_id)?;
        let mut valid_window_ids = vec![];
        if let Some(elements) = application.window_elements() {
            for element in elements {
                if let Ok(window_id) = AccessibilityApi::window_id(&element) {
                    valid_window_ids.push(window_id);
                }
            }
        }

        let mut reaped_count = 0;

        let focused_workspace = self.focused_workspace_mut()?;
        reaped_count += focused_workspace
            .reap_invalid_windows_for_application(process_id, &valid_window_ids)?;

        if reaped_count > 0 {
            tracing::debug!("reaped {reaped_count} invalid window(s)");
        }

        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn add_window_to_container(&mut self, direction: OperationDirection) -> eyre::Result<()> {
        tracing::info!("adding window to container");
        let mouse_follows_focus = self.mouse_follows_focus;

        let workspace = self.focused_workspace_mut()?;
        let len = NonZeroUsize::new(workspace.containers_mut().len())
            .ok_or_eyre("there must be at least one container")?;
        let current_container_idx = workspace.focused_container_idx();

        let is_valid = direction
            .destination(
                workspace.layout.as_boxed_direction().as_ref(),
                workspace.layout_flip,
                workspace.focused_container_idx(),
                len,
            )
            .is_some();

        if is_valid {
            let new_idx = workspace
                .new_idx_for_direction(direction)
                .ok_or_eyre("this is not a valid direction from the current position")?;

            let mut changed_focus = false;

            let adjusted_new_index = if new_idx > current_container_idx
                && !matches!(
                    workspace.layout,
                    Layout::Default(DefaultLayout::Grid)
                        | Layout::Default(DefaultLayout::UltrawideVerticalStack)
                ) {
                workspace.focus_container(new_idx);
                changed_focus = true;
                new_idx.saturating_sub(1)
            } else {
                new_idx
            };

            let mut target_container_is_stack = false;

            if let Some(container) = workspace.containers().get(adjusted_new_index)
                && container.windows().len() > 1
            {
                target_container_is_stack = true;
            }

            if let Some(current) = workspace.focused_container() {
                if current.windows().len() > 1 && !target_container_is_stack {
                    workspace.focus_container(adjusted_new_index);
                    changed_focus = true;
                    workspace.move_window_to_container(current_container_idx)?;
                } else {
                    workspace.move_window_to_container(adjusted_new_index)?;
                }
            }

            if changed_focus && let Some(container) = workspace.focused_container_mut() {
                container.load_focused_window()?;
                if let Some(window) = container.focused_window() {
                    window.focus(mouse_follows_focus)?;
                }
            }

            self.update_focused_workspace(mouse_follows_focus, false)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn remove_window_from_container(&mut self) -> eyre::Result<()> {
        tracing::info!("removing window");

        if self.focused_container()?.windows().len() == 1 {
            bail!("a container must have at least one window");
        }

        let workspace = self.focused_workspace_mut()?;

        workspace.new_container_for_focused_window()?;
        self.update_focused_workspace(self.mouse_follows_focus, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn stop(&mut self, ignore_restore: bool) -> eyre::Result<()> {
        tracing::info!(
            "received stop command, restoring all hidden windows and terminating process"
        );

        // TODO: figure out if we wanna do state restores in macOS
        // let state = &State::from(&*self);
        // std::fs::write(
        //     DATA_DIR.join("komorebi.state.json"),
        //     serde_json::to_string_pretty(&state)?,
        // )?;

        self.restore_all_windows(ignore_restore)?;

        let sockets = SUBSCRIPTION_SOCKETS.lock();
        for path in (*sockets).values() {
            if let Ok(stream) = UnixStream::connect(path) {
                stream.shutdown(Shutdown::Both)?;
            }
        }

        let socket = DATA_DIR.join("komorebi.sock");
        let _ = std::fs::remove_file(socket);

        std::process::exit(0)
    }

    #[tracing::instrument(skip(self))]
    pub fn restore_all_windows(&mut self, ignore_restore: bool) -> eyre::Result<()> {
        tracing::info!("restoring all hidden windows");

        for monitor in self.monitors_mut() {
            for workspace in monitor.workspaces_mut() {
                for containers in workspace.containers_mut() {
                    for window in containers.windows_mut() {
                        if !ignore_restore && let Err(error) = window.restore() {
                            tracing::error!("failed to restore window: {}", error);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn cycle_container_window_in_direction(
        &mut self,
        direction: CycleDirection,
    ) -> eyre::Result<()> {
        tracing::info!("cycling container windows");

        let mouse_follows_focus = self.mouse_follows_focus;

        let container = self.focused_container_mut()?;

        let len = NonZeroUsize::new(container.windows().len())
            .ok_or_eyre("there must be at least one window in a container")?;

        if len.get() == 1 {
            eyre::bail!("there is only one window in this container");
        }

        let current_idx = container.focused_window_idx();
        let next_idx = direction.next_idx(current_idx, len);

        container.focus_window(next_idx);
        container.load_focused_window()?;

        if let Some(window) = container.focused_window() {
            window.focus(mouse_follows_focus)?;
        }

        self.update_focused_workspace(mouse_follows_focus, true)
    }
    #[tracing::instrument(skip(self))]
    pub fn focus_workspace(&mut self, idx: usize) -> eyre::Result<()> {
        tracing::info!("focusing workspace");

        let mouse_follows_focus = self.mouse_follows_focus;
        let monitor = self
            .focused_monitor_mut()
            .ok_or_eyre("there is no workspace")?;

        monitor.focus_workspace(idx)?;
        monitor.load_focused_workspace(mouse_follows_focus)?;

        self.update_focused_workspace(false, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_to_workspace(
        &mut self,
        idx: usize,
        follow: bool,
        direction: Option<OperationDirection>,
    ) -> eyre::Result<()> {
        tracing::info!("moving container");

        let mouse_follows_focus = self.mouse_follows_focus;
        let monitor = self
            .focused_monitor_mut()
            .ok_or_eyre("there is no monitor")?;

        monitor.move_container_to_workspace(idx, follow, direction)?;
        monitor.load_focused_workspace(mouse_follows_focus)?;

        self.update_focused_workspace(mouse_follows_focus, true)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn toggle_monocle(&mut self) -> eyre::Result<()> {
        let workspace = self.focused_workspace()?;
        match workspace.monocle_container {
            None => self.monocle_on()?,
            Some(_) => self.monocle_off()?,
        }

        self.update_focused_workspace(true, true)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn monocle_on(&mut self) -> eyre::Result<()> {
        tracing::info!("enabling monocle");

        let workspace = self.focused_workspace_mut()?;
        workspace.new_monocle_container()?;

        for container in workspace.containers_mut() {
            container.hide(None)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn monocle_off(&mut self) -> eyre::Result<()> {
        tracing::info!("disabling monocle");

        let workspace = self.focused_workspace_mut()?;

        for container in workspace.containers_mut() {
            container.restore()?;
        }

        workspace.reintegrate_monocle_container()
    }

    #[tracing::instrument(skip(self))]
    pub fn toggle_float(&mut self, force_float: bool) -> eyre::Result<()> {
        let window_id = MacosApi::foreground_window_id().ok_or_eyre("no foreground window")?;

        let workspace = self.focused_workspace_mut()?;
        if workspace.monocle_container.is_some() {
            tracing::warn!("ignoring toggle-float command while workspace has a monocle container");
            return Ok(());
        }

        let mut is_floating_window = false;

        for window in workspace.floating_windows() {
            if window.id == window_id {
                is_floating_window = true;
            }
        }

        if is_floating_window && !force_float {
            workspace.layer = WorkspaceLayer::Tiling;
            self.unfloat_window()?;
        } else {
            workspace.layer = WorkspaceLayer::Floating;
            self.float_window()?;
        }

        self.update_focused_workspace(is_floating_window, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn float_window(&mut self) -> eyre::Result<()> {
        tracing::info!("floating window");

        let mouse_follows_focus = self.mouse_follows_focus;
        let work_area = self.focused_monitor_work_area()?;

        let toggle_float_placement = self.window_management_behaviour.toggle_float_placement;

        let workspace = self.focused_workspace_mut()?;
        workspace.new_floating_window()?;

        let window = workspace
            .floating_windows_mut()
            .back_mut()
            .ok_or_eyre("there is no floating window")?;

        if toggle_float_placement.should_center() {
            window.center(&work_area, toggle_float_placement.should_resize())?;
        }
        window.focus(mouse_follows_focus)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn unfloat_window(&mut self) -> eyre::Result<()> {
        tracing::info!("unfloating window");

        let workspace = self.focused_workspace_mut()?;
        workspace.new_container_for_floating_window()
    }

    #[tracing::instrument(skip(self))]
    pub fn resize_window(
        &mut self,
        direction: OperationDirection,
        sizing: Sizing,
        delta: i32,
        update: bool,
    ) -> eyre::Result<()> {
        let mouse_follows_focus = self.mouse_follows_focus;
        let mut focused_monitor_work_area = self.focused_monitor_work_area()?;
        let workspace = self.focused_workspace_mut()?;

        match workspace.layer {
            WorkspaceLayer::Floating => {
                let workspace = self.focused_workspace()?;
                let focused_window_id =
                    MacosApi::foreground_window_id().ok_or_eyre("no foreground window")?;

                let border_offset = 0;
                let border_width = 0;
                focused_monitor_work_area.left += border_offset;
                focused_monitor_work_area.left += border_width;
                focused_monitor_work_area.top += border_offset;
                focused_monitor_work_area.top += border_width;
                focused_monitor_work_area.right -= border_offset * 2;
                focused_monitor_work_area.right -= border_width * 2;
                focused_monitor_work_area.bottom -= border_offset * 2;
                focused_monitor_work_area.bottom -= border_width * 2;

                for window in workspace.floating_windows().iter() {
                    if window.id == focused_window_id {
                        let mut rect = Rect::from(MacosApi::window_rect(&window.element)?);
                        match (direction, sizing) {
                            (OperationDirection::Left, Sizing::Increase) => {
                                if rect.left - delta < focused_monitor_work_area.left {
                                    rect.left = focused_monitor_work_area.left;
                                } else {
                                    rect.left -= delta;
                                }
                            }
                            (OperationDirection::Left, Sizing::Decrease) => {
                                rect.left += delta;
                            }
                            (OperationDirection::Right, Sizing::Increase) => {
                                if rect.left + rect.right + delta * 2
                                    > focused_monitor_work_area.left
                                        + focused_monitor_work_area.right
                                {
                                    rect.right = focused_monitor_work_area.left
                                        + focused_monitor_work_area.right
                                        - rect.left;
                                } else {
                                    rect.right += delta * 2;
                                }
                            }
                            (OperationDirection::Right, Sizing::Decrease) => {
                                rect.right -= delta * 2;
                            }
                            (OperationDirection::Up, Sizing::Increase) => {
                                if rect.top - delta < focused_monitor_work_area.top {
                                    rect.top = focused_monitor_work_area.top;
                                } else {
                                    rect.top -= delta;
                                }
                            }
                            (OperationDirection::Up, Sizing::Decrease) => {
                                rect.top += delta;
                            }
                            (OperationDirection::Down, Sizing::Increase) => {
                                if rect.top + rect.bottom + delta * 2
                                    > focused_monitor_work_area.top
                                        + focused_monitor_work_area.bottom
                                {
                                    rect.bottom = focused_monitor_work_area.top
                                        + focused_monitor_work_area.bottom
                                        - rect.top;
                                } else {
                                    rect.bottom += delta * 2;
                                }
                            }
                            (OperationDirection::Down, Sizing::Decrease) => {
                                rect.bottom -= delta * 2;
                            }
                        }

                        window.set_position(&rect)?;

                        if mouse_follows_focus {
                            MacosApi::center_cursor_in_rect(&rect)?;
                        }

                        break;
                    }
                }
            }
            WorkspaceLayer::Tiling => {
                match workspace.layout {
                    Layout::Default(layout) => {
                        tracing::info!("resizing window");
                        let len = NonZeroUsize::new(workspace.containers().len())
                            .ok_or_eyre("there must be at least one container")?;
                        let focused_idx = workspace.focused_container_idx();
                        let focused_idx_resize = workspace
                            .resize_dimensions
                            .get(focused_idx)
                            .ok_or_eyre("there is no resize adjustment for this container")?;

                        if direction
                            .destination(
                                workspace.layout.as_boxed_direction().as_ref(),
                                workspace.layout_flip,
                                focused_idx,
                                len,
                            )
                            .is_some()
                        {
                            let unaltered = layout.calculate(
                                &focused_monitor_work_area,
                                len,
                                workspace.container_padding,
                                workspace.layout_flip,
                                &[],
                                workspace.focused_container_idx(),
                                workspace.layout_options,
                                &workspace.latest_layout,
                            );

                            let mut direction = direction;

                            // We only ever want to operate on the unflipped Rect positions when resizing, then we
                            // can flip them however they need to be flipped once the resizing has been done
                            if let Some(flip) = workspace.layout_flip {
                                match flip {
                                    Axis::Horizontal => {
                                        if matches!(direction, OperationDirection::Left)
                                            || matches!(direction, OperationDirection::Right)
                                        {
                                            direction = direction.opposite();
                                        }
                                    }
                                    Axis::Vertical => {
                                        if matches!(direction, OperationDirection::Up)
                                            || matches!(direction, OperationDirection::Down)
                                        {
                                            direction = direction.opposite();
                                        }
                                    }
                                    Axis::HorizontalAndVertical => direction = direction.opposite(),
                                }
                            }

                            let resize = layout.resize(
                                unaltered
                                    .get(focused_idx)
                                    .ok_or_eyre("there is no last layout")?,
                                focused_idx_resize,
                                direction,
                                sizing,
                                delta,
                            );

                            workspace.resize_dimensions[focused_idx] = resize;

                            return if update {
                                self.update_focused_workspace(false, false)
                            } else {
                                Ok(())
                            };
                        }

                        tracing::warn!("cannot resize container in this direction");
                    }
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn retile_all(&mut self, preserve_resize_dimensions: bool) -> eyre::Result<()> {
        let offset = self.work_area_offset;

        for monitor in self.monitors_mut() {
            let offset = if monitor.work_area_offset.is_some() {
                monitor.work_area_offset
            } else {
                offset
            };

            let focused_workspace_idx = monitor.focused_workspace_idx();
            monitor.update_workspace_globals(focused_workspace_idx, offset);

            let workspace = monitor
                .focused_workspace_mut()
                .ok_or_eyre("there is no workspace")?;

            // Reset any resize adjustments if we want to force a retile
            if !preserve_resize_dimensions {
                for resize in &mut workspace.resize_dimensions {
                    *resize = None;
                }
            }

            workspace.update()?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn promote_container_to_front(&mut self) -> eyre::Result<()> {
        let workspace = self.focused_workspace_mut()?;

        if matches!(workspace.layout, Layout::Default(DefaultLayout::Grid)) {
            tracing::debug!("ignoring promote command for grid layout");
            return Ok(());
        }

        tracing::info!("promoting container");

        workspace.promote_container()?;
        self.update_focused_workspace(self.mouse_follows_focus, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn promote_focus_to_front(&mut self) -> eyre::Result<()> {
        let workspace = self.focused_workspace_mut()?;

        if matches!(workspace.layout, Layout::Default(DefaultLayout::Grid)) {
            tracing::info!("ignoring promote focus command for grid layout");
            return Ok(());
        }

        tracing::info!("promoting focus");

        let target_idx = match workspace.layout {
            Layout::Default(_) => 0,
        };

        workspace.focus_container(target_idx);
        self.update_focused_workspace(self.mouse_follows_focus, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn flip_layout(&mut self, layout_flip: Axis) -> eyre::Result<()> {
        let workspace = self.focused_workspace_mut()?;

        tracing::info!("flipping layout");

        #[allow(clippy::match_same_arms)]
        match workspace.layout_flip {
            None => {
                workspace.layout_flip = Option::from(layout_flip);
            }
            Some(current_layout_flip) => {
                match current_layout_flip {
                    Axis::Horizontal => match layout_flip {
                        Axis::Horizontal => workspace.layout_flip = None,
                        Axis::Vertical => {
                            workspace.layout_flip = Option::from(Axis::HorizontalAndVertical)
                        }
                        Axis::HorizontalAndVertical => {
                            workspace.layout_flip = Option::from(Axis::HorizontalAndVertical)
                        }
                    },
                    Axis::Vertical => match layout_flip {
                        Axis::Horizontal => {
                            workspace.layout_flip = Option::from(Axis::HorizontalAndVertical)
                        }
                        Axis::Vertical => workspace.layout_flip = None,
                        Axis::HorizontalAndVertical => {
                            workspace.layout_flip = Option::from(Axis::HorizontalAndVertical)
                        }
                    },
                    Axis::HorizontalAndVertical => match layout_flip {
                        Axis::Horizontal => workspace.layout_flip = Option::from(Axis::Vertical),
                        Axis::Vertical => workspace.layout_flip = Option::from(Axis::Horizontal),
                        Axis::HorizontalAndVertical => workspace.layout_flip = None,
                    },
                };
            }
        }

        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn cycle_layout(&mut self, direction: CycleDirection) -> eyre::Result<()> {
        tracing::info!("cycling layout");

        let workspace = self.focused_workspace_mut()?;

        match workspace.layout {
            Layout::Default(current) => {
                let new_layout = match direction {
                    CycleDirection::Previous => current.cycle_previous(),
                    CycleDirection::Next => current.cycle_next(),
                };

                tracing::info!("next layout: {new_layout}");
                workspace.layout = Layout::Default(new_layout);
            }
        }

        self.update_focused_workspace(self.mouse_follows_focus, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn toggle_lock(&mut self) -> eyre::Result<()> {
        let workspace = self.focused_workspace_mut()?;
        if let Some(container) = workspace.focused_container_mut() {
            // Toggle the locked flag
            container.set_locked(!container.locked());
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn toggle_tiling(&mut self) -> eyre::Result<()> {
        let workspace = self.focused_workspace_mut()?;
        workspace.tile = !workspace.tile;
        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_monitor(&mut self, idx: usize) -> eyre::Result<()> {
        tracing::info!("focusing monitor");

        if self.monitors().get(idx).is_some() {
            self.monitors.focus(idx);
        } else {
            eyre::bail!("this is not a valid monitor index");
        }

        Ok(())
    }

    // TODO: see if this logic is the same in macos when I have more than one monitor
    pub fn monitor_idx_in_direction(&self, direction: OperationDirection) -> Option<usize> {
        let current_monitor_size = self.focused_monitor_size().ok()?;

        for (idx, monitor) in self.monitors.elements().iter().enumerate() {
            match direction {
                OperationDirection::Left => {
                    if monitor.size.left + monitor.size.right == current_monitor_size.left {
                        return Option::from(idx);
                    }
                }
                OperationDirection::Right => {
                    if current_monitor_size.right + current_monitor_size.left == monitor.size.left {
                        return Option::from(idx);
                    }
                }
                OperationDirection::Up => {
                    if monitor.size.top + monitor.size.bottom == current_monitor_size.top {
                        return Option::from(idx);
                    }
                }
                OperationDirection::Down => {
                    if current_monitor_size.top + current_monitor_size.bottom == monitor.size.top {
                        return Option::from(idx);
                    }
                }
            }
        }

        None
    }

    pub fn monitor_idx_from_current_pos(&mut self) -> Option<usize> {
        let monitor_id = MacosApi::monitor_from_point(MacosApi::cursor_pos())?;

        for (i, monitor) in self.monitors().iter().enumerate() {
            if monitor.id == monitor_id {
                return Option::from(i);
            }
        }

        // TODO: figure out if we need this on macOS
        // // our hmonitor might be stale, so if we didn't return above, try querying via the latest
        // // info taken from win32_display_data and update our hmonitor while we're at it
        // if let Ok(latest) = MacosApi::monitor(monitor_id) {
        //     for (i, monitor) in self.monitors_mut().iter_mut().enumerate() {
        //         if monitor.device_id() == latest.device_id() {
        //             monitor.set_id(latest.id());
        //             return Option::from(i);
        //         }
        //     }
        // }

        None
    }

    #[tracing::instrument(skip(self))]
    pub fn monitor_workspace_index_by_name(&mut self, name: &str) -> Option<(usize, usize)> {
        tracing::info!("looking up workspace by name");

        for (monitor_idx, monitor) in self.monitors().iter().enumerate() {
            for (workspace_idx, workspace) in monitor.workspaces().iter().enumerate() {
                if let Some(workspace_name) = &workspace.name
                    && workspace_name == name
                {
                    return Option::from((monitor_idx, workspace_idx));
                }
            }
        }

        None
    }

    /// Calculates the direction of a move across monitors given a specific monitor index
    pub fn direction_from_monitor_idx(
        &self,
        target_monitor_idx: usize,
    ) -> Option<OperationDirection> {
        let current_monitor_idx = self.focused_monitor_idx();
        if current_monitor_idx == target_monitor_idx {
            return None;
        }

        let current_monitor_size = self.focused_monitor_size().ok()?;
        let target_monitor_size = self.monitors().get(target_monitor_idx)?.size;

        if target_monitor_size.left + target_monitor_size.right == current_monitor_size.left {
            return Some(OperationDirection::Left);
        }
        if current_monitor_size.right + current_monitor_size.left == target_monitor_size.left {
            return Some(OperationDirection::Right);
        }
        if target_monitor_size.top + target_monitor_size.bottom == current_monitor_size.top {
            return Some(OperationDirection::Up);
        }
        if current_monitor_size.top + current_monitor_size.bottom == target_monitor_size.top {
            return Some(OperationDirection::Down);
        }

        None
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_to_monitor(
        &mut self,
        monitor_idx: usize,
        workspace_idx: Option<usize>,
        follow: bool,
        move_direction: Option<OperationDirection>,
    ) -> eyre::Result<()> {
        tracing::info!("moving container");

        let focused_monitor_idx = self.focused_monitor_idx();

        if focused_monitor_idx == monitor_idx
            && let Some(workspace_idx) = workspace_idx
        {
            return self.move_container_to_workspace(workspace_idx, follow, None);
        }

        let offset = self.work_area_offset;
        let mouse_follows_focus = self.mouse_follows_focus;

        let monitor = self
            .focused_monitor_mut()
            .ok_or_eyre("there is no monitor")?;

        let current_area = monitor.work_area_size;

        let workspace = monitor
            .focused_workspace_mut()
            .ok_or_eyre("there is no workspace")?;

        if workspace.maximized_window.is_some() {
            eyre::bail!("cannot move native maximized window to another monitor or workspace");
        }

        let foreground_window_id =
            MacosApi::foreground_window_id().ok_or_eyre("no foreground window")?;
        let floating_window_index = workspace
            .floating_windows()
            .iter()
            .position(|w| w.id == foreground_window_id);

        let floating_window =
            floating_window_index.and_then(|idx| workspace.floating_windows_mut().remove(idx));
        let container = if floating_window_index.is_none() {
            Some(
                workspace
                    .remove_focused_container()
                    .ok_or_eyre("there is no container")?,
            )
        } else {
            None
        };
        monitor.update_focused_workspace(offset)?;

        let target_monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let target_monitor_work_area_size = target_monitor.work_area_size;

        let mut should_load_workspace = false;
        if let Some(workspace_idx) = workspace_idx
            && workspace_idx != target_monitor.focused_workspace_idx()
        {
            target_monitor.focus_workspace(workspace_idx)?;
            should_load_workspace = true;
        }
        let target_workspace = target_monitor
            .focused_workspace_mut()
            .ok_or_eyre("there is no focused workspace on target monitor")?;

        if target_workspace.monocle_container.is_some() {
            for container in target_workspace.containers_mut() {
                container.restore()?;
            }

            for window in target_workspace.floating_windows_mut() {
                window.restore()?;
            }

            target_workspace.reintegrate_monocle_container()?;
        }

        if let Some(window) = floating_window {
            window.move_to_area(&current_area, &target_monitor_work_area_size)?;
            target_workspace.floating_windows_mut().push_back(window);
            target_workspace.layer = WorkspaceLayer::Floating;
        } else if let Some(container) = container {
            let _container_window_ids =
                container.windows().iter().map(|w| w.id).collect::<Vec<_>>();

            target_workspace.layer = WorkspaceLayer::Tiling;

            if let Some(direction) = move_direction {
                target_monitor.add_container_with_direction(container, workspace_idx, direction)?;
            } else {
                target_monitor.add_container(container, workspace_idx)?;
            }

            if let Some(workspace) = target_monitor.focused_workspace()
                && !workspace.tile
            {
                // TODO: figure out how to construct a Window from just an id here
                // for window_id in container_window_ids {
                //     Window::from(window_id)
                //         .move_to_area(&current_area, &target_monitor.work_area_size)?;
                // }
            }
        } else {
            eyre::bail!("failed to find a window to move");
        }

        if should_load_workspace {
            target_monitor.load_focused_workspace(mouse_follows_focus)?;
        }
        target_monitor.update_focused_workspace(offset)?;

        // this second one is for DPI changes when the target is another monitor
        // if we don't do this the layout on the other monitor could look funny
        // until it is interacted with again
        target_monitor.update_focused_workspace(offset)?;

        if follow {
            self.focus_monitor(monitor_idx)?;
        }

        self.update_focused_workspace(self.mouse_follows_focus, true)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn swap_focused_monitor(&mut self, idx: usize) -> eyre::Result<()> {
        tracing::info!("swapping focused monitor");

        let focused_monitor_idx = self.focused_monitor_idx();
        let mouse_follows_focus = self.mouse_follows_focus;

        self.swap_monitor_workspaces(focused_monitor_idx, idx)?;

        self.update_focused_workspace(mouse_follows_focus, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn swap_monitor_workspaces(
        &mut self,
        first_idx: usize,
        second_idx: usize,
    ) -> eyre::Result<()> {
        tracing::info!("swaping monitors");
        if first_idx == second_idx {
            return Ok(());
        }
        let mouse_follows_focus = self.mouse_follows_focus;
        let offset = self.work_area_offset;
        let first_focused_workspace = {
            let first_monitor = self
                .monitors()
                .get(first_idx)
                .ok_or_eyre("There is no monitor")?;
            first_monitor.focused_workspace_idx()
        };

        let second_focused_workspace = {
            let second_monitor = self
                .monitors()
                .get(second_idx)
                .ok_or_eyre("There is no monitor")?;
            second_monitor.focused_workspace_idx()
        };

        // Swap workspaces between the first and second monitors

        let first_workspaces = self
            .monitors_mut()
            .get_mut(first_idx)
            .ok_or_eyre("There is no monitor")?
            .remove_workspaces();

        let second_workspaces = self
            .monitors_mut()
            .get_mut(second_idx)
            .ok_or_eyre("There is no monitor")?
            .remove_workspaces();

        self.monitors_mut()
            .get_mut(first_idx)
            .ok_or_eyre("There is no monitor")?
            .workspaces_mut()
            .extend(second_workspaces);

        self.monitors_mut()
            .get_mut(second_idx)
            .ok_or_eyre("There is no monitor")?
            .workspaces_mut()
            .extend(first_workspaces);

        // Set the focused workspaces for the first and second monitors
        if let Some(first_monitor) = self.monitors_mut().get_mut(first_idx) {
            first_monitor.update_workspaces_globals(offset);
            first_monitor.focus_workspace(second_focused_workspace)?;
            first_monitor.load_focused_workspace(mouse_follows_focus)?;
        }

        if let Some(second_monitor) = self.monitors_mut().get_mut(second_idx) {
            second_monitor.update_workspaces_globals(offset);
            second_monitor.focus_workspace(first_focused_workspace)?;
            second_monitor.load_focused_workspace(mouse_follows_focus)?;
        }

        self.update_focused_workspace_by_monitor_idx(second_idx)?;
        self.update_focused_workspace_by_monitor_idx(first_idx)
    }

    pub fn update_focused_workspace_by_monitor_idx(&mut self, idx: usize) -> eyre::Result<()> {
        let offset = self.work_area_offset;

        self.monitors_mut()
            .get_mut(idx)
            .ok_or_eyre("there is no monitor")?
            .update_focused_workspace(offset)
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_container_in_cycle_direction(
        &mut self,
        direction: CycleDirection,
    ) -> eyre::Result<()> {
        tracing::info!("focusing container");
        // let mut maximize_next = false;
        let mut monocle_next = false;

        let mouse_follows_focus = self.mouse_follows_focus;

        if self.focused_workspace_mut()?.maximized_window.is_some() {
            // TODO: maximized window stuff
            // maximize_next = true;
            // self.unmaximize_window()?;
        }

        if self.focused_workspace_mut()?.monocle_container.is_some() {
            monocle_next = true;
            self.monocle_off()?;
        }

        let workspace = self.focused_workspace_mut()?;

        let new_idx = workspace
            .new_idx_for_cycle_direction(direction)
            .ok_or_eyre("this is not a valid direction from the current position")?;

        workspace.focus_container(new_idx);

        // if maximize_next {
        //     self.toggle_maximize()?;
        // } else
        if monocle_next {
            self.toggle_monocle()?;
        } else {
            self.focused_window_mut()?.focus(mouse_follows_focus)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_in_cycle_direction(
        &mut self,
        direction: CycleDirection,
    ) -> eyre::Result<()> {
        let workspace = self.focused_workspace_mut()?;
        if workspace.is_focused_window_monocle_or_maximized()? {
            eyre::bail!("ignoring command while active window is in monocle mode or maximized");
        }

        tracing::info!("moving container");

        let current_idx = workspace.focused_container_idx();
        let new_idx = workspace
            .new_idx_for_cycle_direction(direction)
            .ok_or_eyre("this is not a valid direction from the current position")?;

        workspace.swap_containers(current_idx, new_idx);
        workspace.focus_container(new_idx);
        self.update_focused_workspace(self.mouse_follows_focus, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_floating_window_in_cycle_direction(
        &mut self,
        direction: CycleDirection,
    ) -> eyre::Result<()> {
        let mouse_follows_focus = self.mouse_follows_focus;
        let focused_workspace = self.focused_workspace()?;

        let mut target_idx = None;
        let len = focused_workspace.floating_windows().len();

        if len > 1 {
            let focused_window_id =
                MacosApi::foreground_window_id().ok_or_eyre("no foreground window")?;
            for (idx, window) in focused_workspace.floating_windows().iter().enumerate() {
                if window.id == focused_window_id {
                    match direction {
                        CycleDirection::Previous => {
                            if idx == 0 {
                                target_idx = Some(len - 1)
                            } else {
                                target_idx = Some(idx - 1)
                            }
                        }
                        CycleDirection::Next => {
                            if idx == len - 1 {
                                target_idx = Some(0)
                            } else {
                                target_idx = Some(idx - 1)
                            }
                        }
                    }
                }
            }

            if target_idx.is_none() {
                target_idx = Some(0);
            }
        }

        if let Some(idx) = target_idx
            && let Some(window) = focused_workspace.floating_windows().get(idx)
        {
            window.focus(mouse_follows_focus)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn cycle_container_window_index_in_direction(
        &mut self,
        direction: CycleDirection,
    ) -> eyre::Result<()> {
        tracing::info!("cycling container window index");

        let mouse_follows_focus = self.mouse_follows_focus;

        let container =
            if let Some(container) = &mut self.focused_workspace_mut()?.monocle_container {
                container
            } else {
                self.focused_container_mut()?
            };

        let len = NonZeroUsize::new(container.windows().len())
            .ok_or_eyre("there must be at least one window in a container")?;

        if len.get() == 1 {
            eyre::bail!("there is only one window in this container");
        }

        let current_idx = container.focused_window_idx();
        let next_idx = direction.next_idx(current_idx, len);
        container.windows_mut().swap(current_idx, next_idx);

        container.focus_window(next_idx);
        container.load_focused_window()?;

        if let Some(window) = container.focused_window() {
            window.focus(mouse_follows_focus)?;
        }

        self.update_focused_workspace(mouse_follows_focus, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_container_window(&mut self, idx: usize) -> eyre::Result<()> {
        tracing::info!("focusing container window at index {idx}");

        let mouse_follows_focus = self.mouse_follows_focus;

        let container =
            if let Some(container) = &mut self.focused_workspace_mut()?.monocle_container {
                container
            } else {
                self.focused_container_mut()?
            };

        let len = NonZeroUsize::new(container.windows().len())
            .ok_or_eyre("there must be at least one window in a container")?;

        if len.get() == 1 && idx != 0 {
            eyre::bail!("there is only one window in this container");
        }

        if container.windows().get(idx).is_none() {
            eyre::bail!("there is no window in this container at index {idx}");
        }

        container.focus_window(idx);
        container.load_focused_window()?;

        if let Some(window) = container.focused_window() {
            window.focus(mouse_follows_focus)?;
        }

        self.update_focused_workspace(mouse_follows_focus, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn stack_all(&mut self) -> eyre::Result<()> {
        self.unstack_all(false)?;

        tracing::info!("stacking all windows on workspace");

        let workspace = self.focused_workspace_mut()?;

        let mut focused_window_id = None;
        if let Some(container) = workspace.focused_container()
            && let Some(window) = container.focused_window()
        {
            focused_window_id = Some(window.id);
        }

        workspace.focus_container(workspace.containers().len().saturating_sub(1));
        while workspace.focused_container_idx() > 0 {
            workspace.move_window_to_container(0)?;
            workspace.focus_container(workspace.containers().len().saturating_sub(1));
        }

        if let Some(window_id) = focused_window_id {
            workspace.focus_container_by_window(window_id)?;
        }

        self.update_focused_workspace(self.mouse_follows_focus, true)
    }

    #[tracing::instrument(skip(self))]
    pub fn unstack_all(&mut self, update_workspace: bool) -> eyre::Result<()> {
        tracing::info!("unstacking all windows in container");

        let workspace = self.focused_workspace_mut()?;

        let mut focused_window_id = None;
        if let Some(container) = workspace.focused_container()
            && let Some(window) = container.focused_window()
        {
            focused_window_id = Some(window.id);
        }

        let initial_focused_container_index = workspace.focused_container_idx();
        let mut focused_container = workspace.focused_container().cloned();

        while let Some(focused) = &focused_container {
            if focused.windows().len() > 1 {
                workspace.new_container_for_focused_window()?;
                workspace.focus_container(initial_focused_container_index);
                focused_container = workspace.focused_container().cloned();
            } else {
                focused_container = None;
            }
        }

        if let Some(window_id) = focused_window_id {
            workspace.focus_container_by_window(window_id)?;
        }

        if update_workspace {
            self.update_focused_workspace(self.mouse_follows_focus, true)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    pub fn enforce_workspace_rules(&mut self) -> eyre::Result<()> {
        let mut to_move = vec![];

        let focused_monitor_idx = self.focused_monitor_idx();
        let focused_workspace_idx = self
            .monitors()
            .get(focused_monitor_idx)
            .ok_or_eyre("there is no monitor with that index")?
            .focused_workspace_idx();

        // scope mutex locks to avoid deadlock if should_update_focused_workspace evaluates to true
        // at the end of this function
        {
            let workspace_matching_rules = WORKSPACE_MATCHING_RULES.lock();
            let regex_identifiers = REGEX_IDENTIFIERS.lock();
            // Go through all the monitors and workspaces
            for (i, monitor) in self.monitors().iter().enumerate() {
                for (j, workspace) in monitor.workspaces().iter().enumerate() {
                    // And all the visible windows (at the top of a container)
                    for window in workspace.visible_windows().into_iter().flatten() {
                        if let (
                            Some(exe_name),
                            Some(title),
                            Some(role),
                            Some(subrole),
                            Some(path),
                        ) = (
                            window.exe(),
                            window.title(),
                            window.role(),
                            window.subrole(),
                            window.path(),
                        ) {
                            for rule in &*workspace_matching_rules {
                                let matched = match &rule.matching_rule {
                                    MatchingRule::Simple(r) => should_act_individual(
                                        &title,
                                        &exe_name,
                                        &[&role, &subrole],
                                        &path.to_string_lossy(),
                                        r,
                                        &regex_identifiers,
                                    ),
                                    MatchingRule::Composite(r) => {
                                        let mut composite_results = vec![];
                                        for identifier in r {
                                            composite_results.push(should_act_individual(
                                                &title,
                                                &exe_name,
                                                &[&role, &subrole],
                                                &path.to_string_lossy(),
                                                identifier,
                                                &regex_identifiers,
                                            ));
                                        }

                                        composite_results.iter().all(|&x| x)
                                    }
                                };

                                if matched {
                                    let floating = workspace.floating_windows().contains(window);

                                    let mut already_moved_window_handles =
                                        self.already_moved_window_handles.lock();

                                    if rule.initial_only {
                                        if !already_moved_window_handles.contains(&window.id) {
                                            already_moved_window_handles.insert(window.id);

                                            self.add_window_handle_to_move_based_on_workspace_rule(
                                                &window
                                                    .title()
                                                    .ok_or_eyre("could not read window title")?,
                                                window.id,
                                                i,
                                                j,
                                                rule.monitor_index,
                                                rule.workspace_index,
                                                floating,
                                                &mut to_move,
                                            );
                                        }
                                    } else {
                                        self.add_window_handle_to_move_based_on_workspace_rule(
                                            &window
                                                .title()
                                                .ok_or_eyre("could not read window title")?,
                                            window.id,
                                            i,
                                            j,
                                            rule.monitor_index,
                                            rule.workspace_index,
                                            floating,
                                            &mut to_move,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Only retain operations where the target is not the current workspace
        to_move.retain(|op| !op.is_target(focused_monitor_idx, focused_workspace_idx));
        // Only retain operations where the rule has not already been enforced
        to_move.retain(|op| !op.is_enforced());

        let mut should_update_focused_workspace = false;
        let mut removed_windows = HashMap::new();

        // Parse the operation and remove any windows that are not placed according to their rules
        for op in &to_move {
            let target_area = self
                .monitors_mut()
                .get_mut(op.target_monitor_idx)
                .ok_or_eyre("there is no monitor with that index")?
                .work_area_size;

            let origin_monitor = self
                .monitors_mut()
                .get_mut(op.origin_monitor_idx)
                .ok_or_eyre("there is no monitor with that index")?;

            let origin_area = origin_monitor.work_area_size;

            let origin_workspace = origin_monitor
                .workspaces_mut()
                .get_mut(op.origin_workspace_idx)
                .ok_or_eyre("there is no workspace with that index")?;

            let mut window = origin_workspace.remove_window(op.window_id)?;

            // If it is a floating window move it to the target area
            if op.floating {
                window.move_to_area(&origin_area, &target_area)?;
            }

            // Hide the window we are about to remove if it is on the currently focused workspace
            if op.is_origin(focused_monitor_idx, focused_workspace_idx) {
                window.hide()?;
                should_update_focused_workspace = true;
            }

            removed_windows.insert(window.id, window);
        }

        // Parse the operation again and associate those removed windows with the workspace that
        // their rules have defined for them
        for op in &to_move {
            let window = removed_windows
                .get_mut(&op.window_id)
                .ok_or_eyre("there is no window")?;

            let target_monitor = self
                .monitors_mut()
                .get_mut(op.target_monitor_idx)
                .ok_or_eyre("there is no monitor with that index")?;

            // The very first time this fn is called, the workspace might not even exist yet
            if target_monitor
                .workspaces()
                .get(op.target_workspace_idx)
                .is_none()
            {
                // If it doesn't, let's make sure it does for the next step
                target_monitor.ensure_workspace_count(op.target_workspace_idx + 1);
            }

            let target_workspace = target_monitor
                .workspaces_mut()
                .get_mut(op.target_workspace_idx)
                .ok_or_eyre("there is no workspace with that index")?;

            if op.floating {
                target_workspace
                    .floating_windows_mut()
                    // TODO: not sure about this clone
                    .push_back(window.clone());
            } else {
                //TODO(alex-ds13): should this take into account the target workspace
                //`window_container_behaviour`?
                //In the case above a floating window should always be moved as floating,
                //because it was set as so either manually by the user or by a
                //`floating_applications` rule so it should stay that way. But a tiled window
                //when moving to another workspace by a `workspace_rule` should honor that
                //workspace `window_container_behaviour` in my opinion! Maybe this should be done
                //on the `new_container_for_window` function instead.
                // TODO: not sure about this clone
                target_workspace.new_container_for_window(window)?;
            }
        }

        // Only re-tile the focused workspace if we need to
        if should_update_focused_workspace {
            self.update_focused_workspace(false, false)?;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(skip(self), level = "debug")]
    fn add_window_handle_to_move_based_on_workspace_rule(
        &self,
        window_title: &String,
        window_id: u32,
        origin_monitor_idx: usize,
        origin_workspace_idx: usize,
        target_monitor_idx: usize,
        target_workspace_idx: usize,
        floating: bool,
        to_move: &mut Vec<EnforceWorkspaceRuleOp>,
    ) {
        tracing::trace!(
            "{} should be on monitor {}, workspace {}",
            window_title,
            target_monitor_idx,
            target_workspace_idx
        );

        // Create an operation outline and save it for later in the fn
        to_move.push(EnforceWorkspaceRuleOp {
            window_id,
            origin_monitor_idx,
            origin_workspace_idx,
            target_monitor_idx,
            target_workspace_idx,
            floating,
        });
    }

    #[tracing::instrument(skip(self))]
    pub fn reload_static_configuration(&mut self, pathbuf: &PathBuf) -> eyre::Result<()> {
        tracing::info!("reloading static configuration");
        StaticConfig::reload(pathbuf, self)
    }

    #[tracing::instrument(skip(self))]
    pub fn handle_unmanaged_window_behaviour(&self) -> eyre::Result<()> {
        if matches!(
            self.unmanaged_window_operation_behaviour,
            OperationBehaviour::NoOp
        ) {
            let workspace = self.focused_workspace()?;
            let focused_hwnd =
                MacosApi::foreground_window_id().ok_or_eyre("there is no foreground window")?;
            if !workspace.contains_managed_window(focused_hwnd) {
                bail!("ignoring commands while active window is not managed by komorebi");
            }
        }

        Ok(())
    }

    pub fn update_known_window_ids(&mut self) {
        tracing::trace!("updating list of known window_ids");
        let mut known_window_ids = HashMap::new();
        for (m_idx, monitor) in self.monitors().iter().enumerate() {
            for (w_idx, workspace) in monitor.workspaces().iter().enumerate() {
                for container in workspace.containers() {
                    for window in container.windows() {
                        known_window_ids.insert(window.id, (m_idx, w_idx));
                    }
                }

                for window in workspace.floating_windows() {
                    known_window_ids.insert(window.id, (m_idx, w_idx));
                }

                if let Some(window) = &workspace.maximized_window {
                    known_window_ids.insert(window.id, (m_idx, w_idx));
                }

                if let Some(container) = &workspace.monocle_container {
                    for window in container.windows() {
                        known_window_ids.insert(window.id, (m_idx, w_idx));
                    }
                }
            }
        }

        if self.known_window_ids != known_window_ids {
            // Store new window_ids
            self.known_window_ids = known_window_ids;
        }
    }

    pub fn window_management_behaviour(
        &self,
        monitor_idx: usize,
        workspace_idx: usize,
    ) -> WindowManagementBehaviour {
        if let Some(monitor) = self.monitors().get(monitor_idx)
            && let Some(workspace) = monitor.workspaces().get(workspace_idx)
        {
            let current_behaviour = if let Some(behaviour) = workspace.window_container_behaviour {
                if workspace.containers().is_empty()
                    && matches!(behaviour, WindowContainerBehaviour::Append)
                {
                    // You can't append to an empty workspace
                    WindowContainerBehaviour::Create
                } else {
                    behaviour
                }
            } else if workspace.containers().is_empty()
                && matches!(
                    self.window_management_behaviour.current_behaviour,
                    WindowContainerBehaviour::Append
                )
            {
                // You can't append to an empty workspace
                WindowContainerBehaviour::Create
            } else {
                self.window_management_behaviour.current_behaviour
            };

            let float_override = if let Some(float_override) = workspace.float_override {
                float_override
            } else {
                self.window_management_behaviour.float_override
            };

            let floating_layer_behaviour =
                if let Some(behaviour) = workspace.floating_layer_behaviour {
                    behaviour
                } else {
                    monitor
                        .floating_layer_behaviour
                        .unwrap_or(self.window_management_behaviour.floating_layer_behaviour)
                };

            // If the workspace layer is `Floating` and the floating layer behaviour should
            // float then change floating_layer_override to true so that new windows spawn
            // as floating
            let floating_layer_override = matches!(workspace.layer, WorkspaceLayer::Floating)
                && floating_layer_behaviour.should_float();

            return WindowManagementBehaviour {
                current_behaviour,
                float_override,
                floating_layer_override,
                floating_layer_behaviour,
                toggle_float_placement: self.window_management_behaviour.toggle_float_placement,
                floating_layer_placement: self.window_management_behaviour.floating_layer_placement,
                float_override_placement: self.window_management_behaviour.float_override_placement,
                float_rule_placement: self.window_management_behaviour.float_rule_placement,
            };
        }

        WindowManagementBehaviour {
            current_behaviour: WindowContainerBehaviour::Create,
            float_override: self.window_management_behaviour.float_override,
            floating_layer_override: self.window_management_behaviour.floating_layer_override,
            floating_layer_behaviour: self.window_management_behaviour.floating_layer_behaviour,
            toggle_float_placement: self.window_management_behaviour.toggle_float_placement,
            floating_layer_placement: self.window_management_behaviour.floating_layer_placement,
            float_override_placement: self.window_management_behaviour.float_override_placement,
            float_rule_placement: self.window_management_behaviour.float_rule_placement,
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn set_workspace_padding(
        &mut self,
        monitor_idx: usize,
        workspace_idx: usize,
        size: i32,
    ) -> eyre::Result<()> {
        tracing::info!("setting workspace padding");

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let workspace = monitor
            .workspaces_mut()
            .get_mut(workspace_idx)
            .ok_or_eyre("there is no monitor")?;

        workspace.workspace_padding = Option::from(size);

        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn set_workspace_name(
        &mut self,
        monitor_idx: usize,
        workspace_idx: usize,
        name: String,
    ) -> eyre::Result<()> {
        tracing::info!("setting workspace name");

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let workspace = monitor
            .workspaces_mut()
            .get_mut(workspace_idx)
            .ok_or_eyre("there is no monitor")?;

        workspace.name = Option::from(name.clone());
        // monitor.workspace_names.insert(workspace_idx, name);

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn set_container_padding(
        &mut self,
        monitor_idx: usize,
        workspace_idx: usize,
        size: i32,
    ) -> eyre::Result<()> {
        tracing::info!("setting container padding");

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let workspace = monitor
            .workspaces_mut()
            .get_mut(workspace_idx)
            .ok_or_eyre("there is no monitor")?;

        workspace.container_padding = Option::from(size);

        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn set_workspace_tiling(
        &mut self,
        monitor_idx: usize,
        workspace_idx: usize,
        tile: bool,
    ) -> eyre::Result<()> {
        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let workspace = monitor
            .workspaces_mut()
            .get_mut(workspace_idx)
            .ok_or_eyre("there is no monitor")?;

        workspace.tile = tile;

        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn add_workspace_layout_default_rule(
        &mut self,
        monitor_idx: usize,
        workspace_idx: usize,
        at_container_count: usize,
        layout: DefaultLayout,
    ) -> eyre::Result<()> {
        tracing::info!("setting workspace layout");

        let focused_monitor_idx = self.focused_monitor_idx();

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let focused_workspace_idx = monitor.focused_workspace_idx();

        let workspace = monitor
            .workspaces_mut()
            .get_mut(workspace_idx)
            .ok_or_eyre("there is no monitor")?;

        let rules: &mut Vec<(usize, Layout)> = &mut workspace.layout_rules;
        rules.retain(|pair| pair.0 != at_container_count);
        rules.push((at_container_count, Layout::Default(layout)));
        rules.sort_by(|a, b| a.0.cmp(&b.0));

        // If this is the focused workspace on a non-focused screen, let's update it
        if focused_monitor_idx != monitor_idx && focused_workspace_idx == workspace_idx {
            workspace.update()?;
            Ok(())
        } else {
            Ok(self.update_focused_workspace(false, false)?)
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn clear_workspace_layout_rules(
        &mut self,
        monitor_idx: usize,
        workspace_idx: usize,
    ) -> eyre::Result<()> {
        tracing::info!("clearing workspace layout rules");

        let focused_monitor_idx = self.focused_monitor_idx();

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        let focused_workspace_idx = monitor.focused_workspace_idx();

        let workspace = monitor
            .workspaces_mut()
            .get_mut(workspace_idx)
            .ok_or_eyre("there is no monitor")?;

        let rules: &mut Vec<(usize, Layout)> = &mut workspace.layout_rules;
        rules.clear();

        // If this is the focused workspace on a non-focused screen, let's update it
        if focused_monitor_idx != monitor_idx && focused_workspace_idx == workspace_idx {
            workspace.update()?;
            Ok(())
        } else {
            Ok(self.update_focused_workspace(false, false)?)
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn adjust_workspace_padding(
        &mut self,
        sizing: Sizing,
        adjustment: i32,
    ) -> eyre::Result<()> {
        tracing::info!("adjusting workspace padding");

        let workspace = self.focused_workspace_mut()?;

        let padding = workspace
            .workspace_padding
            .ok_or_eyre("there is no workspace padding")?;

        workspace.workspace_padding = Option::from(sizing.adjust_by(padding, adjustment));

        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn adjust_container_padding(
        &mut self,
        sizing: Sizing,
        adjustment: i32,
    ) -> eyre::Result<()> {
        tracing::info!("adjusting container padding");

        let workspace = self.focused_workspace_mut()?;

        let padding = workspace
            .container_padding
            .ok_or_eyre("there is no container padding")?;

        workspace.container_padding = Option::from(sizing.adjust_by(padding, adjustment));

        self.update_focused_workspace(false, false)
    }

    #[tracing::instrument(skip(self))]
    pub fn ensure_workspaces_for_monitor(
        &mut self,
        monitor_idx: usize,
        workspace_count: usize,
    ) -> eyre::Result<()> {
        tracing::info!("ensuring workspace count");

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        monitor.ensure_workspace_count(workspace_count);

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn ensure_named_workspaces_for_monitor(
        &mut self,
        monitor_idx: usize,
        names: &Vec<String>,
    ) -> eyre::Result<()> {
        tracing::info!("ensuring workspace count");

        let monitor = self
            .monitors_mut()
            .get_mut(monitor_idx)
            .ok_or_eyre("there is no monitor")?;

        monitor.ensure_workspace_count(names.len());

        for (workspace_idx, name) in names.iter().enumerate() {
            if let Some(workspace) = monitor.workspaces_mut().get_mut(workspace_idx) {
                workspace.name = Option::from(name.clone());
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
            .ok_or_eyre("there is no workspace")?;

        {
            let target_monitor: &mut Monitor = self
                .monitors_mut()
                .get_mut(idx)
                .ok_or_eyre("there is no monitor")?;

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

    #[tracing::instrument(skip(self))]
    pub fn manage_focused_window(&mut self) -> eyre::Result<()> {
        let element = MacosApi::foreground_window().ok_or_eyre("there is no foreground window")?;
        let window_id = MacosApi::foreground_window_id();
        if let Some(process_id) = AdhocWindow::process_id(&element)
            && let Some(event) = WindowManagerEvent::from_system_notification(
                SystemNotification::Manual(ManualNotification::Manage),
                process_id,
                window_id,
            )
        {
            UNMANAGED_WINDOW_IDS
                .lock()
                .retain(|id| *id != window_id.unwrap_or_default());

            window_manager_event_listener::send_notification(event);
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn unmanage_focused_window(&mut self) -> eyre::Result<()> {
        let element = MacosApi::foreground_window().ok_or_eyre("there is no foreground window")?;
        let window_id = MacosApi::foreground_window_id();
        if let Some(process_id) = AdhocWindow::process_id(&element)
            && let Some(event) = WindowManagerEvent::from_system_notification(
                SystemNotification::Manual(ManualNotification::Unmanage),
                process_id,
                window_id,
            )
        {
            window_manager_event_listener::send_notification(event);
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn transfer_window(
        &mut self,
        origin: (usize, usize, u32),
        target: (usize, usize, usize),
    ) -> eyre::Result<()> {
        let (origin_monitor_idx, origin_workspace_idx, window_id) = origin;
        let (target_monitor_idx, target_workspace_idx, target_container_idx) = target;

        let origin_workspace = self
            .monitors_mut()
            .get_mut(origin_monitor_idx)
            .ok_or_eyre("cannot get monitor idx")?
            .workspaces_mut()
            .get_mut(origin_workspace_idx)
            .ok_or_eyre("cannot get workspace idx")?;

        let origin_container_idx = origin_workspace
            .container_for_window(window_id)
            .and_then(|c| origin_workspace.containers().iter().position(|cc| cc == c));

        if let Some(origin_container_idx) = origin_container_idx {
            // Moving normal container window
            self.transfer_container(
                (
                    origin_monitor_idx,
                    origin_workspace_idx,
                    origin_container_idx,
                ),
                (
                    target_monitor_idx,
                    target_workspace_idx,
                    target_container_idx,
                ),
            )?;
        } else if let Some(idx) = origin_workspace
            .floating_windows()
            .iter()
            .position(|w| w.id == window_id)
        {
            // Moving floating window
            // There is no need to physically move the floating window between areas with
            // `move_to_area` because the user already did that, so we only need to transfer the
            // window to the target `floating_windows`
            if let Some(floating_window) = origin_workspace.floating_windows_mut().remove(idx) {
                let target_workspace = self
                    .monitors_mut()
                    .get_mut(target_monitor_idx)
                    .ok_or_eyre("there is no monitor at this idx")?
                    .focused_workspace_mut()
                    .ok_or_eyre("there is no focused workspace for this monitor")?;

                target_workspace
                    .floating_windows_mut()
                    .push_back(floating_window);
            }
        } else if origin_workspace
            .monocle_container
            .as_ref()
            .and_then(|monocle| monocle.focused_window().map(|w| w.id == window_id))
            .unwrap_or_default()
        {
            // Moving monocle container
            if let Some(monocle_idx) = origin_workspace.monocle_container_restore_idx {
                let origin_workspace = self
                    .monitors_mut()
                    .get_mut(origin_monitor_idx)
                    .ok_or_eyre("there is no monitor at this idx")?
                    .workspaces_mut()
                    .get_mut(origin_workspace_idx)
                    .ok_or_eyre("there is no workspace for this monitor")?;

                for container in origin_workspace.containers_mut() {
                    container.restore()?;
                }

                origin_workspace.reintegrate_monocle_container()?;

                self.transfer_container(
                    (origin_monitor_idx, origin_workspace_idx, monocle_idx),
                    (
                        target_monitor_idx,
                        target_workspace_idx,
                        target_container_idx,
                    ),
                )?;
                // TODO: don't think this is needed on macOS
                // After we restore the origin workspace, some windows that were cloacked
                // by the monocle might now be uncloacked which would trigger a workspace
                // reconciliation since the focused monitor would be different from origin.
                // That workspace reconciliation would focus the window on the origin monitor.
                // So we need to ignore the uncloak events produced by the origin workspace
                // restore to avoid that issue.
                // self.uncloack_to_ignore = uncloack_amount;
            }
            // TODO: this probably won't be used on macOS
        } else if origin_workspace
            .maximized_window
            .as_ref()
            .map(|max| max.id == window_id)
            .unwrap_or_default()
        {
            // Moving maximized_window
            // TODO: not sure if we'll support maximized windows on macOS
            // if let Some(maximized_idx) = origin_workspace.maximized_window_restore_idx {
            //     self.focus_monitor(origin_monitor_idx)?;
            //     let origin_monitor = self
            //         .focused_monitor_mut()
            //         .ok_or_eyre("there is no origin monitor")?;
            //     origin_monitor.focus_workspace(origin_workspace_idx)?;
            //     self.unmaximize_window()?;
            //     self.focus_monitor(target_monitor_idx)?;
            //     let target_monitor = self
            //         .focused_monitor_mut()
            //         .ok_or_eyre("there is no target monitor")?;
            //     target_monitor.focus_workspace(target_workspace_idx)?;
            //
            //     self.transfer_container(
            //         (origin_monitor_idx, origin_workspace_idx, maximized_idx),
            //         (
            //             target_monitor_idx,
            //             target_workspace_idx,
            //             target_container_idx,
            //         ),
            //     )?;
            // }
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn transfer_container(
        &mut self,
        origin: (usize, usize, usize),
        target: (usize, usize, usize),
    ) -> eyre::Result<()> {
        let (origin_monitor_idx, origin_workspace_idx, origin_container_idx) = origin;
        let (target_monitor_idx, target_workspace_idx, target_container_idx) = target;

        let origin_container = self
            .monitors_mut()
            .get_mut(origin_monitor_idx)
            .ok_or_eyre("there is no monitor at this index")?
            .workspaces_mut()
            .get_mut(origin_workspace_idx)
            .ok_or_eyre("there is no workspace at this index")?
            .remove_container(origin_container_idx)
            .ok_or_eyre("there is no container at this index")?;

        let target_workspace = self
            .monitors_mut()
            .get_mut(target_monitor_idx)
            .ok_or_eyre("there is no monitor at this index")?
            .workspaces_mut()
            .get_mut(target_workspace_idx)
            .ok_or_eyre("there is no workspace at this index")?;

        target_workspace
            .containers_mut()
            .insert(target_container_idx, origin_container);

        target_workspace.focus_container(target_container_idx);

        Ok(())
    }
}

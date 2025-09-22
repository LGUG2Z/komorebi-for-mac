use crate::DEFAULT_CONTAINER_PADDING;
use crate::DEFAULT_WORKSPACE_PADDING;
use crate::container::Container;
use crate::core::FloatingLayerBehaviour;
use crate::core::default_layout::DefaultLayout;
use crate::core::layout::Layout;
use crate::core::operation_direction::OperationDirection;
use crate::core::rect::Rect;
use crate::macos_api::MacosApi;
use crate::ring::Ring;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceGlobals;
use crate::workspace::WorkspaceLayer;
use color_eyre::eyre;
use color_eyre::eyre::OptionExt;
use serde::Deserialize;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;

#[derive(Debug, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub alphanumeric_serial_number: String,
    pub manufacturer_id: String,
    pub product_name: String,
    pub legacy_manufacturer_id: String,
    pub product_id: String,
    pub serial_number: u32,
    pub week_of_manufacture: String,
    pub year_of_manufacture: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MonitorInformation {
    pub id: u32,
    pub device: String,
    pub serial_number_id: String,
    pub size: Rect,
}

impl From<&Monitor> for MonitorInformation {
    fn from(value: &Monitor) -> Self {
        Self {
            id: value.id,
            device: value.device.clone(),
            serial_number_id: value.serial_number_id.clone(),
            size: value.size,
        }
    }
}

impl_ring_elements!(Monitor, Workspace);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Monitor {
    pub id: u32,
    pub device: String,
    pub serial_number_id: String,
    pub workspaces: Ring<Workspace>,
    pub size: Rect,
    pub work_area_offset: Option<Rect>,
    pub work_area_size: Rect,
    pub window_based_work_area_offset: Option<Rect>,
    pub window_based_work_area_offset_limit: isize,
    pub container_padding: Option<i32>,
    pub workspace_padding: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_focused_workspace: Option<usize>,
    pub floating_layer_behaviour: Option<FloatingLayerBehaviour>,
}

impl Monitor {
    #[allow(clippy::field_reassign_with_default)]
    pub fn new(
        id: u32,
        size: Rect,
        work_area_size: Rect,
        device: &str,
        serial_number_id: &str,
    ) -> Self {
        let mut workspaces = Ring::default();
        workspaces.elements_mut().push_back(Workspace::default());

        Self {
            id,
            device: device.to_string(),
            serial_number_id: serial_number_id.to_string(),
            workspaces,
            size,
            work_area_offset: None,
            work_area_size,
            window_based_work_area_offset: Some(Rect {
                left: 500,
                top: 0,
                right: 1000,
                bottom: 0,
            }),
            window_based_work_area_offset_limit: 1,
            container_padding: None,
            workspace_padding: None,
            last_focused_workspace: None,
            floating_layer_behaviour: None,
        }
    }

    pub fn update_focused_workspace(&mut self, offset: Option<Rect>) -> eyre::Result<()> {
        let offset = if self.work_area_offset.is_some() {
            self.work_area_offset
        } else {
            offset
        };

        let focused_workspace_idx = self.focused_workspace_idx();
        self.update_workspace_globals(focused_workspace_idx, offset);
        self.focused_workspace_mut()
            .ok_or_eyre("there is no workspace")?
            .update()?;

        Ok(())
    }

    /// Updates the `globals` field of workspace with index `workspace_idx`
    pub fn update_workspace_globals(&mut self, workspace_idx: usize, offset: Option<Rect>) {
        let container_padding = self
            .container_padding
            .or(Some(DEFAULT_CONTAINER_PADDING.load(Ordering::Relaxed)));
        let workspace_padding = self
            .workspace_padding
            .or(Some(DEFAULT_WORKSPACE_PADDING.load(Ordering::Relaxed)));
        let (border_width, border_offset) = (0, 0);
        let work_area = self.work_area_size;
        let work_area_offset = self.work_area_offset.or(offset);
        let window_based_work_area_offset = self.window_based_work_area_offset;
        let window_based_work_area_offset_limit = self.window_based_work_area_offset_limit;
        let floating_layer_behaviour = self.floating_layer_behaviour;

        if let Some(workspace) = self.workspaces_mut().get_mut(workspace_idx) {
            workspace.globals = WorkspaceGlobals {
                container_padding,
                workspace_padding,
                border_width,
                border_offset,
                work_area,
                work_area_offset,
                window_based_work_area_offset,
                window_based_work_area_offset_limit,
                floating_layer_behaviour,
            }
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn focus_workspace(&mut self, idx: usize) -> eyre::Result<()> {
        tracing::info!("focusing workspace");

        {
            let workspaces = self.workspaces_mut();

            if workspaces.get(idx).is_none() {
                workspaces.resize(idx + 1, Workspace::default());
            }
            self.last_focused_workspace = Some(self.workspaces.focused_idx());
            self.workspaces.focus(idx);
        }

        Ok(())
    }

    pub fn load_focused_workspace(&mut self, mouse_follows_focus: bool) -> eyre::Result<()> {
        let focused_idx = self.focused_workspace_idx();
        for (i, workspace) in self.workspaces_mut().iter_mut().enumerate() {
            if i == focused_idx {
                workspace.restore(mouse_follows_focus)?;
            } else {
                workspace.hide(None)?;
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_to_workspace(
        &mut self,
        target_workspace_idx: usize,
        follow: bool,
        direction: Option<OperationDirection>,
    ) -> eyre::Result<()> {
        let workspace = self
            .focused_workspace_mut()
            .ok_or_eyre("there is no workspace")?;

        // if workspace.maximized_window().is_some() {
        //     eyre::bail!("cannot move native maximized window to another monitor or workspace");
        // }

        let foreground_hwnd =
            MacosApi::foreground_window_id().ok_or_eyre("no foreground window")?;
        let floating_window_index = workspace
            .floating_windows()
            .iter()
            .position(|w| w.id == foreground_hwnd);

        if let Some(idx) = floating_window_index {
            if let Some(window) = workspace.floating_windows_mut().remove(idx) {
                let workspaces = self.workspaces_mut();
                #[allow(clippy::option_if_let_else)]
                let target_workspace = match workspaces.get_mut(target_workspace_idx) {
                    None => {
                        workspaces.resize(target_workspace_idx + 1, Workspace::default());
                        workspaces.get_mut(target_workspace_idx).unwrap()
                    }
                    Some(workspace) => workspace,
                };

                target_workspace.floating_windows_mut().push_back(window);
                target_workspace.layer = WorkspaceLayer::Floating;
            }
        } else {
            let container = workspace
                .remove_focused_container()
                .ok_or_eyre("there is no container")?;

            let workspaces = self.workspaces_mut();

            #[allow(clippy::option_if_let_else)]
            let target_workspace = match workspaces.get_mut(target_workspace_idx) {
                None => {
                    workspaces.resize(target_workspace_idx + 1, Workspace::default());
                    workspaces.get_mut(target_workspace_idx).unwrap()
                }
                Some(workspace) => workspace,
            };

            if target_workspace.monocle_container.is_some() {
                for container in target_workspace.containers_mut() {
                    container.restore()?;
                }

                for window in target_workspace.floating_windows_mut() {
                    window.restore()?;
                }

                target_workspace.reintegrate_monocle_container()?;
            }

            target_workspace.layer = WorkspaceLayer::Tiling;

            if let Some(direction) = direction {
                self.add_container_with_direction(
                    container,
                    Some(target_workspace_idx),
                    direction,
                )?;
            } else {
                target_workspace.add_container_to_back(container);
            }
        }

        if follow {
            self.focus_workspace(target_workspace_idx)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn move_container_to_workspace1(
        &mut self,
        target_workspace_idx: usize,
        follow: bool,
        direction: Option<OperationDirection>,
    ) -> eyre::Result<()> {
        let workspace = self
            .focused_workspace_mut()
            .ok_or_eyre("there is no workspace")?;

        let container = workspace
            .remove_focused_container()
            .ok_or_eyre("there is no container")?;

        let workspaces = self.workspaces_mut();

        #[allow(clippy::option_if_let_else)]
        let target_workspace = match workspaces.get_mut(target_workspace_idx) {
            None => {
                workspaces.resize(target_workspace_idx + 1, Workspace::default());
                workspaces.get_mut(target_workspace_idx).unwrap()
            }
            Some(workspace) => workspace,
        };

        if target_workspace.monocle_container.is_some() {
            for container in target_workspace.containers_mut() {
                container.restore()?;
            }

            target_workspace.reintegrate_monocle_container()?;
        }

        if let Some(direction) = direction {
            self.add_container_with_direction(container, Some(target_workspace_idx), direction)?;
        } else {
            target_workspace.add_container_to_back(container);
        }

        if follow {
            self.focus_workspace(target_workspace_idx)?;
        }

        Ok(())
    }

    /// Adds a container to this `Monitor` using the move direction to calculate if the container
    /// should be added in front of all containers, in the back or in place of the focused
    /// container, moving the rest along. The move direction should be from the origin monitor
    /// towards the target monitor or from the origin workspace towards the target workspace.
    pub fn add_container_with_direction(
        &mut self,
        container: Container,
        workspace_idx: Option<usize>,
        direction: OperationDirection,
    ) -> eyre::Result<()> {
        let workspace = if let Some(idx) = workspace_idx {
            self.workspaces_mut()
                .get_mut(idx)
                .ok_or_eyre(format!("there is no workspace at index {}", idx))?
        } else {
            self.focused_workspace_mut()
                .ok_or_eyre("there is no workspace")?
        };

        match direction {
            OperationDirection::Left => {
                // insert the container into the workspace on the monitor at the back (or rightmost position)
                // if we are moving across a boundary to the left (back = right side of the target)
                match workspace.layout {
                    Layout::Default(layout) => match layout {
                        DefaultLayout::RightMainVerticalStack => {
                            workspace.add_container_to_front(container);
                        }
                        DefaultLayout::UltrawideVerticalStack => {
                            if workspace.containers().len() == 1 {
                                workspace.insert_container_at_idx(0, container);
                            } else {
                                workspace.add_container_to_back(container);
                            }
                        }
                        _ => {
                            workspace.add_container_to_back(container);
                        }
                    },
                }
            }
            OperationDirection::Right => {
                // insert the container into the workspace on the monitor at the front (or leftmost position)
                // if we are moving across a boundary to the right (front = left side of the target)
                match workspace.layout {
                    Layout::Default(layout) => {
                        let target_index = layout.leftmost_index(workspace.containers().len());

                        match layout {
                            DefaultLayout::RightMainVerticalStack
                            | DefaultLayout::UltrawideVerticalStack => {
                                if workspace.containers().len() == 1 {
                                    workspace.add_container_to_back(container);
                                } else {
                                    workspace.insert_container_at_idx(target_index, container);
                                }
                            }
                            _ => {
                                workspace.insert_container_at_idx(target_index, container);
                            }
                        }
                    }
                }
            }
            OperationDirection::Up | OperationDirection::Down => {
                // insert the container into the workspace on the monitor at the position
                // where the currently focused container on that workspace is
                workspace.insert_container_at_idx(workspace.focused_container_idx(), container);
            }
        };

        Ok(())
    }

    pub fn update_workspaces_globals(&mut self, offset: Option<Rect>) {
        let container_padding = self
            .container_padding
            .or(Some(DEFAULT_CONTAINER_PADDING.load(Ordering::SeqCst)));
        let workspace_padding = self
            .workspace_padding
            .or(Some(DEFAULT_WORKSPACE_PADDING.load(Ordering::SeqCst)));
        let (border_width, border_offset) = (0, 0);
        let work_area = self.work_area_size;
        let work_area_offset = self.work_area_offset.or(offset);
        let window_based_work_area_offset = self.window_based_work_area_offset;
        let window_based_work_area_offset_limit = self.window_based_work_area_offset_limit;
        let floating_layer_behaviour = self.floating_layer_behaviour;

        for workspace in self.workspaces_mut() {
            workspace.globals = WorkspaceGlobals {
                container_padding,
                workspace_padding,
                border_width,
                border_offset,
                work_area,
                work_area_offset,
                window_based_work_area_offset,
                window_based_work_area_offset_limit,
                floating_layer_behaviour,
            }
        }
    }

    pub fn remove_workspace_by_idx(&mut self, idx: usize) -> Option<Workspace> {
        if idx < self.workspaces().len() {
            return self.workspaces_mut().remove(idx);
        }

        if idx == 0 {
            self.workspaces_mut().push_back(Workspace::default());
        } else {
            self.focus_workspace(idx.saturating_sub(1)).ok()?;
        };

        None
    }

    pub fn add_container(
        &mut self,
        container: Container,
        workspace_idx: Option<usize>,
    ) -> eyre::Result<()> {
        let workspace = if let Some(idx) = workspace_idx {
            self.workspaces_mut()
                .get_mut(idx)
                .ok_or_eyre(format!("there is no workspace at index {}", idx))?
        } else {
            self.focused_workspace_mut()
                .ok_or_eyre("there is no workspace")?
        };

        workspace.add_container_to_back(container);

        Ok(())
    }

    pub fn remove_workspaces(&mut self) -> VecDeque<Workspace> {
        self.workspaces_mut().drain(..).collect()
    }

    pub fn ensure_workspace_count(&mut self, ensure_count: usize) {
        if self.workspaces().len() < ensure_count {
            self.workspaces_mut()
                .resize(ensure_count, Workspace::default());
        }
    }
}

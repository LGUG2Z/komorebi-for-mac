use crate::DEFAULT_CONTAINER_PADDING;
use crate::DEFAULT_WORKSPACE_PADDING;
use crate::core::rect::Rect;
use crate::ring::Ring;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceGlobals;
use color_eyre::eyre;
use color_eyre::eyre::eyre;
use std::sync::atomic::Ordering;

impl_ring_elements!(Monitor, Workspace);

#[derive(Debug)]
pub struct Monitor {
    pub id: u32,
    pub workspaces: Ring<Workspace>,
    pub size: Rect,
    pub work_area_offset: Option<Rect>,
    pub work_area_size: Rect,
    pub window_based_work_area_offset: Option<Rect>,
    pub window_based_work_area_offset_limit: isize,
    pub container_padding: Option<i32>,
    pub workspace_padding: Option<i32>,
}

impl Monitor {
    #[allow(clippy::field_reassign_with_default)]
    pub fn new(id: u32, size: Rect, work_area_size: Rect) -> Self {
        let mut workspaces = Ring::default();
        workspaces.elements_mut().push_back(Workspace::default());

        Self {
            id,
            workspaces,
            size,
            work_area_offset: None,
            work_area_size,
            window_based_work_area_offset: None,
            window_based_work_area_offset_limit: 0,
            container_padding: None,
            workspace_padding: None,
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
            .ok_or_else(|| eyre!("there is no workspace"))?
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
            }
        }
    }
}

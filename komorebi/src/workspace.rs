use crate::core::arrangement::Axis;
use crate::core::default_layout::DefaultLayout;
use crate::core::default_layout::LayoutOptions;
use crate::core::layout::Layout;
use crate::core::rect::Rect;
use crate::ring::Ring;
use crate::window_manager::Container;
use color_eyre::eyre;
use color_eyre::eyre::eyre;
use std::num::NonZeroUsize;

impl_ring_elements!(Workspace, Container);

#[derive(Debug)]
pub struct Workspace {
    pub containers: Ring<Container>,
    pub workspace_padding: Option<i32>,
    pub container_padding: Option<i32>,
    pub resize_dimensions: Vec<Option<Rect>>,
    pub layout: Layout,
    pub work_area: Rect,
    pub work_area_offset: Option<Rect>,
    pub latest_layout: Vec<Rect>,
    pub layout_flip: Option<Axis>,
    pub layout_options: Option<LayoutOptions>,
    pub globals: WorkspaceGlobals,
    pub tile: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            containers: Default::default(),
            workspace_padding: None,
            container_padding: None,
            resize_dimensions: vec![],
            layout: Layout::Default(DefaultLayout::UltrawideVerticalStack),
            work_area: Default::default(),
            work_area_offset: None,
            latest_layout: vec![],
            layout_flip: None,
            layout_options: None,
            globals: Default::default(),
            tile: true,
        }
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq)]
/// Settings setup either by the parent monitor or by the `WindowManager`
pub struct WorkspaceGlobals {
    pub container_padding: Option<i32>,
    pub workspace_padding: Option<i32>,
    pub border_width: i32,
    pub border_offset: i32,
    pub work_area: Rect,
    pub work_area_offset: Option<Rect>,
    pub window_based_work_area_offset: Option<Rect>,
    pub window_based_work_area_offset_limit: isize,
}

impl Workspace {
    pub fn update(&mut self) -> eyre::Result<()> {
        // make sure we are never holding on to empty containers
        self.containers_mut().retain(|c| !c.windows().is_empty());

        let container_padding = self
            .container_padding
            .or(self.globals.container_padding)
            .unwrap_or_default();
        let workspace_padding = self
            .workspace_padding
            .or(self.globals.workspace_padding)
            .unwrap_or_default();
        // let border_width = self.globals.border_width;
        // let border_offset = self.globals.border_offset;
        let work_area = self.globals.work_area;
        let work_area_offset = self.work_area_offset.or(self.globals.work_area_offset);
        // let window_based_work_area_offset = self.globals.window_based_work_area_offset;
        // let window_based_work_area_offset_limit =
        //     self.globals.window_based_work_area_offset_limit;

        let mut adjusted_work_area = work_area_offset.map_or_else(
            || self.work_area,
            |offset| {
                let mut with_offset = work_area;
                with_offset.left += offset.left;
                with_offset.top += offset.top;
                with_offset.right -= offset.right;
                with_offset.bottom -= offset.bottom;

                with_offset
            },
        );

        adjusted_work_area.add_padding(workspace_padding);

        #[allow(clippy::collapsible_if)]
        if self.tile {
            if !self.containers().is_empty() {
                let mut layouts = self.layout.as_boxed_arrangement().calculate(
                    &adjusted_work_area,
                    NonZeroUsize::new(self.containers().len()).ok_or_else(|| {
                        eyre!(
                            "there must be at least one container to calculate a workspace layout"
                        )
                    })?,
                    Some(container_padding),
                    self.layout_flip,
                    &self.resize_dimensions,
                    self.focused_container_idx(),
                    self.layout_options,
                    &self.latest_layout,
                );

                let containers = self.containers_mut();

                for (i, container) in containers.iter_mut().enumerate() {
                    if let Some(layout) = layouts.get_mut(i) {
                        layout.add_padding(container_padding);
                        for window in container.windows() {
                            if let Err(error) = window.set_position(layout) {
                                tracing::warn!("failed to set window position: {error}");
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

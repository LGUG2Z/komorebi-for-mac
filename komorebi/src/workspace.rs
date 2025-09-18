use crate::container::Container;
use crate::core::arrangement::Axis;
use crate::core::default_layout::DefaultLayout;
use crate::core::default_layout::LayoutOptions;
use crate::core::layout::Layout;
use crate::core::operation_direction::OperationDirection;
use crate::core::rect::Rect;
use crate::lockable_sequence::LockableSequence;
use crate::ring::Ring;
use crate::window::Window;
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
            || work_area,
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

    #[tracing::instrument(skip(self))]
    pub fn focus_container(&mut self, idx: usize) {
        tracing::info!("focusing container");

        self.containers.focus(idx);
    }

    pub fn swap_containers(&mut self, i: usize, j: usize) {
        self.containers.elements_mut().swap_respecting_locks(i, j);
        self.focus_container(j);
    }

    pub fn new_idx_for_direction(&self, direction: OperationDirection) -> Option<usize> {
        let len = NonZeroUsize::new(self.containers().len())?;

        direction.destination(
            self.layout.as_boxed_direction().as_ref(),
            self.layout_flip,
            self.focused_container_idx(),
            len,
        )
    }

    pub fn container_idx_for_window(&self, window_id: u32) -> Option<usize> {
        let mut idx = None;
        for (i, x) in self.containers().iter().enumerate() {
            if x.contains_window(window_id) {
                idx = Option::from(i);
            }
        }

        idx
    }

    pub fn focus_container_by_window(&mut self, window_id: u32) -> eyre::Result<()> {
        let container_idx = self
            .container_idx_for_window(window_id)
            .ok_or_else(|| eyre!("there is no container/window"))?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_else(|| eyre!("there is no container"))?;

        let window_idx = container
            .idx_for_window(window_id)
            .ok_or_else(|| eyre!("there is no window"))?;

        let mut should_load = false;

        if container.focused_window_idx() != window_idx {
            should_load = true
        }

        container.focus_window(window_idx);

        if should_load {
            container.load_focused_window()?;
        }

        self.focus_container(container_idx);

        Ok(())
    }

    pub fn new_container_for_window(&mut self, window: &Window) -> eyre::Result<()> {
        let next_idx = if self.containers().is_empty() {
            0
        } else {
            self.focused_container_idx() + 1
        };

        let mut container = Container::default();
        container.add_window(window)?;

        self.insert_container_at_idx(next_idx, container);

        Ok(())
    }

    // this fn respects locked container indexes - we should use it for pretty much everything
    // except monocle and maximize toggles
    pub fn insert_container_at_idx(&mut self, idx: usize, container: Container) -> usize {
        let insertion_idx = self
            .containers_mut()
            .insert_respecting_locks(idx, container);

        if insertion_idx > self.resize_dimensions.len() {
            self.resize_dimensions.push(None);
        } else {
            self.resize_dimensions.insert(insertion_idx, None);
        }

        self.focus_container(insertion_idx);

        insertion_idx
    }

    pub fn reap_invalid_windows_for_application(
        &mut self,
        process_id: i32,
        valid_window_ids: &[u32],
    ) -> eyre::Result<()> {
        let mut invalid_window_ids = vec![];
        for container in self.containers() {
            if let Some(focused_window) = container.focused_window()
                && focused_window.application.process_id == process_id
                && !valid_window_ids.contains(&focused_window.id)
            {
                invalid_window_ids.push(focused_window.id);
            }
        }

        for window_id in invalid_window_ids {
            self.remove_window(window_id)?;
        }

        Ok(())
    }

    pub fn remove_window(&mut self, window_id: u32) -> eyre::Result<Window> {
        let container_idx = self
            .container_idx_for_window(window_id)
            .ok_or_else(|| eyre!("there is no window"))?;

        let container = self
            .containers_mut()
            .get_mut(container_idx)
            .ok_or_else(|| eyre!("there is no container"))?;

        let window_idx = container
            .windows()
            .iter()
            .position(|window| window.id == window_id)
            .ok_or_else(|| eyre!("there is no window"))?;

        let window = container
            .remove_window_by_idx(window_idx)
            .ok_or_else(|| eyre!("there is no window"))?;

        if container.windows().is_empty() {
            self.remove_container_by_idx(container_idx);
            self.focus_previous_container();
        } else {
            container.load_focused_window()?;
            if let Some(window) = container.focused_window() {
                window.focus(false)?;
            }
        }

        Ok(window)
    }

    // this fn respects locked container indexes - we should use it for pretty much everything
    // except monocle and maximize toggles
    pub fn remove_container_by_idx(&mut self, idx: usize) -> Option<Container> {
        let container = self.containers_mut().remove_respecting_locks(idx);

        if idx < self.resize_dimensions.len() {
            self.resize_dimensions.remove(idx);
        }

        container
    }

    pub fn focus_previous_container(&mut self) {
        let focused_idx = self.focused_container_idx();
        self.focus_container(focused_idx.saturating_sub(1));
    }

    pub fn contains_window(&self, window_id: u32) -> bool {
        for container in self.containers() {
            if container.contains_window(window_id) {
                return true;
            }
        }

        false
    }

    // this is what we use for stacking
    pub fn move_window_to_container(&mut self, target_container_idx: usize) -> eyre::Result<()> {
        let focused_idx = self.focused_container_idx();

        let container = self
            .focused_container_mut()
            .ok_or_else(|| eyre!("there is no container"))?;

        let window = container
            .remove_focused_window()
            .ok_or_else(|| eyre!("there is no window"))?;

        // This is a little messy
        let adjusted_target_container_index = if container.windows().is_empty() {
            self.remove_container_by_idx(focused_idx);

            if focused_idx < target_container_idx {
                target_container_idx.saturating_sub(1)
            } else {
                target_container_idx
            }
        } else {
            container.load_focused_window()?;
            target_container_idx
        };

        let target_container = self
            .containers_mut()
            .get_mut(adjusted_target_container_index)
            .ok_or_else(|| eyre!("there is no container"))?;

        target_container.add_window(&window)?;

        self.focus_container(adjusted_target_container_index);
        self.focused_container_mut()
            .ok_or_else(|| eyre!("there is no container"))?
            .load_focused_window()?;

        Ok(())
    }

    pub fn new_container_for_focused_window(&mut self) -> eyre::Result<()> {
        let focused_container_idx = self.focused_container_idx();

        let container = self
            .focused_container_mut()
            .ok_or_else(|| eyre!("there is no container"))?;

        let window = container
            .remove_focused_window()
            .ok_or_else(|| eyre!("there is no window"))?;

        if container.windows().is_empty() {
            self.remove_container_by_idx(focused_container_idx);
        } else {
            container.load_focused_window()?;
        }

        self.new_container_for_window(&window)?;

        let mut container = Container::default();
        container.add_window(&window)?;
        Ok(())
    }
}

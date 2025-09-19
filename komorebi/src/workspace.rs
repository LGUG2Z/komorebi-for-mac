use crate::container::Container;
use crate::core::FloatingLayerBehaviour;
use crate::core::WindowContainerBehaviour;
use crate::core::arrangement::Axis;
use crate::core::default_layout::DefaultLayout;
use crate::core::default_layout::LayoutOptions;
use crate::core::layout::Layout;
use crate::core::operation_direction::OperationDirection;
use crate::core::rect::Rect;
use crate::lockable_sequence::LockableSequence;
use crate::macos_api::MacosApi;
use crate::ring::Ring;
use crate::window::Window;
use color_eyre::eyre;
use color_eyre::eyre::eyre;
use serde::Deserialize;
use serde::Serialize;
use std::fmt::Display;
use std::fmt::Formatter;
use std::num::NonZeroUsize;

#[derive(Debug, Clone)]
pub struct Workspace {
    pub containers: Ring<Container>,
    pub monocle_container: Option<Container>,
    pub monocle_container_restore_idx: Option<usize>,
    pub maximized_window: Option<Window>,
    pub maximized_window_restore_idx: Option<usize>,
    pub floating_windows: Ring<Window>,
    pub layout: Layout,
    pub layout_options: Option<LayoutOptions>,
    pub layout_rules: Vec<(usize, Layout)>,
    pub layout_flip: Option<Axis>,
    pub workspace_padding: Option<i32>,
    pub container_padding: Option<i32>,
    pub latest_layout: Vec<Rect>,
    pub resize_dimensions: Vec<Option<Rect>>,
    pub tile: bool,
    pub work_area_offset: Option<Rect>,
    pub apply_window_based_work_area_offset: bool,
    pub window_container_behaviour: Option<WindowContainerBehaviour>,
    pub window_container_behaviour_rules: Option<Vec<(usize, WindowContainerBehaviour)>>,
    pub float_override: Option<bool>,
    pub globals: WorkspaceGlobals,
    pub layer: WorkspaceLayer,
    pub floating_layer_behaviour: Option<FloatingLayerBehaviour>,
}

#[derive(Debug, Default, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkspaceLayer {
    #[default]
    Tiling,
    Floating,
}

impl Display for WorkspaceLayer {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceLayer::Tiling => write!(f, "Tiling"),
            WorkspaceLayer::Floating => write!(f, "Floating"),
        }
    }
}

impl_ring_elements!(Workspace, Container);
impl_ring_elements!(Workspace, Window, "floating_window");

impl Default for Workspace {
    fn default() -> Self {
        Self {
            containers: Default::default(),
            monocle_container: None,
            monocle_container_restore_idx: None,
            maximized_window: None,
            maximized_window_restore_idx: None,
            workspace_padding: None,
            container_padding: None,
            resize_dimensions: vec![],
            layout: Layout::Default(DefaultLayout::UltrawideVerticalStack),
            work_area_offset: None,
            latest_layout: vec![],
            layout_flip: None,
            layout_options: None,
            layout_rules: vec![],
            globals: Default::default(),
            layer: Default::default(),
            tile: true,
            apply_window_based_work_area_offset: true,
            window_container_behaviour: None,
            window_container_behaviour_rules: None,
            floating_windows: Default::default(),
            float_override: None,
            floating_layer_behaviour: None,
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
        if let Some(container) = self.monocle_container.as_mut()
            && let Some(window_idx) = container
                .windows()
                .iter()
                .position(|window| window.id == window_id)
        {
            let window = container
                .remove_window_by_idx(window_idx)
                .ok_or_else(|| eyre!("there is no window"))?;

            if container.windows().is_empty() {
                self.monocle_container = None;
                self.monocle_container_restore_idx = None;
            }

            for c in self.containers_mut() {
                c.restore()?;
            }

            return Ok(window);
        }

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

    pub fn is_empty(&self) -> bool {
        self.containers().is_empty()
            && self.maximized_window.is_none()
            && self.monocle_container.is_none()
            && self.floating_windows().is_empty()
    }

    pub fn contains_window(&self, window_id: u32) -> bool {
        for container in self.containers() {
            if container.contains_window(window_id) {
                return true;
            }
        }

        if let Some(container) = &self.monocle_container
            && container.contains_window(window_id)
        {
            return true;
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

    pub fn hide(&mut self, omit: Option<u32>) -> eyre::Result<()> {
        for window in self.floating_windows_mut().iter_mut().rev() {
            let mut should_hide = omit.is_none();

            if !should_hide
                && let Some(omit) = omit
                && omit != window.id
            {
                should_hide = true
            }

            if should_hide {
                window.hide()?;
            }
        }

        for container in self.containers_mut() {
            container.hide(omit)?;
        }

        // if let Some(window) = self.maximized_window() {
        //     window.hide();
        // }

        if let Some(container) = &mut self.monocle_container {
            container.hide(omit)?;
        }

        Ok(())
    }

    pub fn restore(&mut self, mouse_follows_focus: bool) -> eyre::Result<()> {
        if let Some(container) = &mut self.monocle_container {
            container.restore()?;

            if let Some(window) = container.focused_window() {
                window.focus(mouse_follows_focus)?;
            }
        }

        let idx = self.focused_container_idx();
        let mut to_focus = None;

        for (i, container) in self.containers().iter().enumerate() {
            if let Some(window) = container.focused_window()
                && idx == i
            {
                to_focus = Option::from(window.clone());
            }
        }

        for container in self.containers_mut() {
            container.restore()?;
        }

        if let Some(container) = self.focused_container_mut() {
            container.focus_window(container.focused_window_idx());
        }

        for window in self.floating_windows_mut() {
            window.restore()?;
        }
        // Do this here to make sure that an error doesn't stop the restoration of other windows
        // Maximised windows and floating windows should always be drawn at the top of the Z order
        // when switching to a workspace
        if let Some(window) = to_focus {
            if
            /* self.maximized_window().is_none() && */
            matches!(self.layer, WorkspaceLayer::Tiling) {
                window.focus(mouse_follows_focus)?;
            // } else if let Some(maximized_window) = self.maximized_window() {
            //     maximized_window.restore();
            //     maximized_window.focus(mouse_follows_focus)?;
            } else if let Some(floating_window) = self.focused_floating_window() {
                floating_window.focus(mouse_follows_focus)?;
            }
        // } else if let Some(maximized_window) = self.maximized_window() {
        //     maximized_window.restore();
        //     maximized_window.focus(mouse_follows_focus)?;
        } else if let Some(floating_window) = self.focused_floating_window() {
            floating_window.focus(mouse_follows_focus)?;
        }

        Ok(())
    }

    pub fn remove_focused_container(&mut self) -> Option<Container> {
        let focused_idx = self.focused_container_idx();
        let container = self.remove_container_by_idx(focused_idx);
        self.focus_previous_container();

        container
    }

    pub fn promote_container(&mut self) -> eyre::Result<()> {
        let resize = self.resize_dimensions.remove(0);
        let container = self
            .remove_focused_container()
            .ok_or_else(|| eyre!("there is no container"))?;

        let primary_idx = match self.layout {
            Layout::Default(_) => 0,
        };

        let insertion_idx = self.insert_container_at_idx(primary_idx, container);
        self.resize_dimensions[insertion_idx] = resize;
        self.focus_container(primary_idx);

        Ok(())
    }

    pub fn add_container_to_back(&mut self, container: Container) {
        self.containers_mut().push_back(container);
        self.focus_last_container();
    }

    pub fn add_container_to_front(&mut self, container: Container) {
        self.containers_mut().push_front(container);
        self.focus_first_container();
    }

    fn focus_last_container(&mut self) {
        self.focus_container(self.containers().len().saturating_sub(1));
    }

    fn focus_first_container(&mut self) {
        self.focus_container(0);
    }

    pub fn new_monocle_container(&mut self) -> eyre::Result<()> {
        let focused_idx = self.focused_container_idx();

        // we shouldn't use remove_container_by_idx here because it doesn't make sense for
        // monocle and maximized toggles which take over the whole screen before being reinserted
        // at the same index to respect locked container indexes
        let container = self
            .containers_mut()
            .remove(focused_idx)
            .ok_or_else(|| eyre!("there is no container"))?;

        // We don't remove any resize adjustments for a monocle, because when this container is
        // inevitably reintegrated, it would be weird if it doesn't go back to the dimensions
        // it had before

        self.monocle_container = Option::from(container);
        self.monocle_container_restore_idx = Option::from(focused_idx);
        self.focus_previous_container();

        self.monocle_container
            .as_mut()
            .ok_or_else(|| eyre!("there is no monocle container"))?
            .load_focused_window()?;

        Ok(())
    }

    pub fn reintegrate_monocle_container(&mut self) -> eyre::Result<()> {
        let restore_idx = self
            .monocle_container_restore_idx
            .ok_or_else(|| eyre!("there is no monocle restore index"))?;

        let container = self
            .monocle_container
            .as_ref()
            .ok_or_else(|| eyre!("there is no monocle container"))?;

        let container = container.clone();
        if restore_idx >= self.containers().len() {
            self.containers_mut()
                .resize(restore_idx, Container::default());
        }

        // we shouldn't use insert_container_at_index here because it doesn't make sense for
        // monocle and maximized toggles which take over the whole screen before being reinserted
        // at the same index to respect locked container indexes
        self.containers_mut().insert(restore_idx, container);
        self.focus_container(restore_idx);
        self.focused_container_mut()
            .ok_or_else(|| eyre!("there is no container"))?
            .load_focused_window()?;

        self.monocle_container = None;
        self.monocle_container_restore_idx = None;

        Ok(())
    }

    pub fn new_floating_window(&mut self) -> eyre::Result<()> {
        let window = if let Some(monocle_container) = &mut self.monocle_container {
            let window = monocle_container
                .remove_focused_window()
                .ok_or_else(|| eyre!("there is no window"))?;

            if monocle_container.windows().is_empty() {
                self.monocle_container = None;
                self.monocle_container_restore_idx = None;
            } else {
                monocle_container.load_focused_window()?;
            }

            window
        } else {
            let focused_idx = self.focused_container_idx();

            let container = self
                .focused_container_mut()
                .ok_or_else(|| eyre!("there is no container"))?;

            let window = container
                .remove_focused_window()
                .ok_or_else(|| eyre!("there is no window"))?;

            if container.windows().is_empty() {
                self.remove_container_by_idx(focused_idx);

                if focused_idx == self.containers().len() {
                    self.focus_container(focused_idx.saturating_sub(1));
                }
            } else {
                container.load_focused_window()?;
            }

            window
        };

        self.floating_windows_mut().push_back(window);

        Ok(())
    }

    pub fn new_container_for_floating_window(&mut self) -> eyre::Result<()> {
        let focused_idx = self.focused_container_idx();
        let window = self
            .remove_focused_floating_window()
            .ok_or_else(|| eyre!("there is no floating window"))?;

        let mut container = Container::default();
        container.add_window(&window)?;

        self.insert_container_at_idx(focused_idx, container);

        Ok(())
    }

    pub fn remove_focused_floating_window(&mut self) -> Option<Window> {
        let window_id = MacosApi::foreground_window_id()?;

        let mut idx = None;
        for (i, window) in self.floating_windows().iter().enumerate() {
            if window_id == window.id {
                idx = Option::from(i);
            }
        }

        match idx {
            None => None,
            Some(idx) => {
                if self.floating_windows().get(idx).is_some() {
                    self.focus_previous_floating_window();
                    self.floating_windows_mut().remove(idx)
                } else {
                    None
                }
            }
        }
    }

    pub fn focus_previous_floating_window(&mut self) {
        let focused_idx = self.focused_floating_window_idx();
        self.focus_floating_window(focused_idx.saturating_sub(1));
    }

    pub fn focus_floating_window(&mut self, idx: usize) {
        tracing::info!("focusing floating window");

        self.floating_windows.focus(idx);
    }
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
        let border_width = self.globals.border_width;
        let border_offset = self.globals.border_offset;
        let work_area = self.globals.work_area;
        let work_area_offset = self.work_area_offset.or(self.globals.work_area_offset);
        let window_based_work_area_offset = self.globals.window_based_work_area_offset;
        let window_based_work_area_offset_limit = self.globals.window_based_work_area_offset_limit;

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

        if (self.containers().len() <= window_based_work_area_offset_limit as usize
            || self.monocle_container.is_some() && window_based_work_area_offset_limit > 0)
            && self.apply_window_based_work_area_offset
        {
            adjusted_work_area = window_based_work_area_offset.map_or_else(
                || adjusted_work_area,
                |offset| {
                    let mut with_offset = adjusted_work_area;
                    with_offset.left += offset.left;
                    with_offset.top += offset.top;
                    with_offset.right -= offset.right;
                    with_offset.bottom -= offset.bottom;

                    with_offset
                },
            );
        }

        adjusted_work_area.add_padding(workspace_padding);

        self.enforce_resize_constraints();

        if !self.layout_rules.is_empty() {
            let mut updated_layout = None;

            for (threshold, layout) in &self.layout_rules {
                if self.containers().len() >= *threshold {
                    updated_layout = Option::from(layout.clone());
                }
            }

            if let Some(updated_layout) = updated_layout {
                self.layout = updated_layout;
            }
        }

        if let Some(window_container_behaviour_rules) = &self.window_container_behaviour_rules {
            let mut updated_behaviour = None;
            for (threshold, behaviour) in window_container_behaviour_rules {
                if self.containers().len() >= *threshold {
                    updated_behaviour = Option::from(*behaviour);
                }
            }

            self.window_container_behaviour = updated_behaviour;
        }

        if self.tile {
            if let Some(container) = self.monocle_container.as_mut() {
                if let Some(window) = container.focused_window_mut() {
                    adjusted_work_area.add_padding(container_padding);
                    adjusted_work_area.add_padding(border_offset);
                    adjusted_work_area.add_padding(border_width);
                    window.set_position(&adjusted_work_area)?;
                };
            } else if !self.containers().is_empty() {
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
                        layout.add_padding(border_offset);
                        layout.add_padding(border_width);

                        for window in container.windows() {
                            window.set_position(layout)?;
                        }
                    }
                }

                self.latest_layout = layouts;
            }
        }

        // Always make sure that the length of the resize dimensions vec is the same as the
        // number of layouts / containers. This should never actually truncate as the remove_window
        // function takes care of cleaning up resize dimensions when destroying empty containers
        let container_count = self.containers().len();

        // since monocle is a toggle, we never want to truncate the resize dimensions since it will
        // almost always be toggled off and the container will be reintegrated into layout
        //
        // without this check, if there are exactly two containers, when one is toggled to monocle
        // the resize dimensions will be truncated to len == 1, and when it is reintegrated, if it
        // had a resize adjustment before, that will have been lost
        if self.monocle_container.is_none() {
            self.resize_dimensions.resize(container_count, None);
        }

        Ok(())
    }
}

impl Workspace {
    fn enforce_resize_constraints(&mut self) {
        match self.layout {
            Layout::Default(DefaultLayout::BSP) => self.enforce_resize_constraints_for_bsp(),
            Layout::Default(DefaultLayout::Columns) => self.enforce_resize_for_columns(),
            Layout::Default(DefaultLayout::Rows) => self.enforce_resize_for_rows(),
            Layout::Default(DefaultLayout::VerticalStack) => {
                self.enforce_resize_for_vertical_stack();
            }
            Layout::Default(DefaultLayout::RightMainVerticalStack) => {
                self.enforce_resize_for_right_vertical_stack();
            }
            Layout::Default(DefaultLayout::HorizontalStack) => {
                self.enforce_resize_for_horizontal_stack();
            }
            Layout::Default(DefaultLayout::UltrawideVerticalStack) => {
                self.enforce_resize_for_ultrawide();
            }
            Layout::Default(DefaultLayout::Scrolling) => {
                self.enforce_resize_for_scrolling();
            }
            _ => self.enforce_no_resize(),
        }
    }

    fn enforce_resize_constraints_for_bsp(&mut self) {
        for (i, rect) in self.resize_dimensions.iter_mut().enumerate() {
            if let Some(rect) = rect {
                // Even containers can't be resized to the bottom
                if i % 2 == 0 {
                    rect.bottom = 0;
                    // Odd containers can't be resized to the right
                } else {
                    rect.right = 0;
                }
            }
        }

        // The first container can never be resized to the left or the top
        if let Some(Some(first)) = self.resize_dimensions.first_mut() {
            first.top = 0;
            first.left = 0;
        }

        // The last container can never be resized to the bottom or the right
        if let Some(Some(last)) = self.resize_dimensions.last_mut() {
            last.bottom = 0;
            last.right = 0;
        }
    }

    fn enforce_resize_for_columns(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                let len = resize_dimensions.len();
                for (i, rect) in resize_dimensions.iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.top = 0;
                        rect.bottom = 0;

                        if i == 0 {
                            rect.left = 0;
                        }
                        if i == len - 1 {
                            rect.right = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_rows(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                let len = resize_dimensions.len();
                for (i, rect) in resize_dimensions.iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.left = 0;
                        rect.right = 0;

                        if i == 0 {
                            rect.top = 0;
                        }
                        if i == len - 1 {
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_vertical_stack(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            // Single window can not be resized at all
            0 | 1 => self.enforce_no_resize(),
            _ => {
                // Zero is actually on the left
                if let Some(mut left) = resize_dimensions[0] {
                    left.top = 0;
                    left.bottom = 0;
                    left.left = 0;
                }

                // Handle stack on the right
                let stack_size = resize_dimensions[1..].len();
                for (i, rect) in resize_dimensions[1..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        // No containers can resize to the right
                        rect.right = 0;

                        // First container in stack cant resize up
                        if i == 0 {
                            rect.top = 0;
                        } else if i == stack_size - 1 {
                            // Last cant be resized to the bottom
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_right_vertical_stack(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            // Single window can not be resized at all
            0 | 1 => self.enforce_no_resize(),
            _ => {
                // Zero is actually on the right
                if let Some(mut left) = resize_dimensions[1] {
                    left.top = 0;
                    left.bottom = 0;
                    left.right = 0;
                }

                // Handle stack on the right
                let stack_size = resize_dimensions[1..].len();
                for (i, rect) in resize_dimensions[1..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        // No containers can resize to the left
                        rect.left = 0;

                        // First container in stack cant resize up
                        if i == 0 {
                            rect.top = 0;
                        } else if i == stack_size - 1 {
                            // Last cant be resized to the bottom
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_horizontal_stack(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                if let Some(mut left) = resize_dimensions[0] {
                    left.top = 0;
                    left.left = 0;
                    left.right = 0;
                }

                let stack_size = resize_dimensions[1..].len();
                for (i, rect) in resize_dimensions[1..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.bottom = 0;

                        if i == 0 {
                            rect.left = 0;
                        }
                        if i == stack_size - 1 {
                            rect.right = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_ultrawide(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            // Single window can not be resized at all
            0 | 1 => self.enforce_no_resize(),
            // Two windows can only be resized in the middle
            2 => {
                // Zero is actually on the right
                if let Some(mut right) = resize_dimensions[0] {
                    right.top = 0;
                    right.bottom = 0;
                    right.right = 0;
                }

                // One is on the left
                if let Some(mut left) = resize_dimensions[1] {
                    left.top = 0;
                    left.bottom = 0;
                    left.left = 0;
                }
            }
            // Three or more windows means 0 is in center, 1 is at the left, 2.. are a vertical
            // stack on the right
            _ => {
                // Central can be resized left or right
                if let Some(mut right) = resize_dimensions[0] {
                    right.top = 0;
                    right.bottom = 0;
                }

                // Left one can only be resized to the right
                if let Some(mut left) = resize_dimensions[1] {
                    left.top = 0;
                    left.bottom = 0;
                    left.left = 0;
                }

                // Handle stack on the right
                let stack_size = resize_dimensions[2..].len();
                for (i, rect) in resize_dimensions[2..].iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        // No containers can resize to the right
                        rect.right = 0;

                        // First container in stack cant resize up
                        if i == 0 {
                            rect.top = 0;
                        } else if i == stack_size - 1 {
                            // Last cant be resized to the bottom
                            rect.bottom = 0;
                        }
                    }
                }
            }
        }
    }

    fn enforce_resize_for_scrolling(&mut self) {
        let resize_dimensions = &mut self.resize_dimensions;
        match resize_dimensions.len() {
            0 | 1 => self.enforce_no_resize(),
            _ => {
                let len = resize_dimensions.len();

                for (i, rect) in resize_dimensions.iter_mut().enumerate() {
                    if let Some(rect) = rect {
                        rect.top = 0;
                        rect.bottom = 0;

                        if i == 0 {
                            rect.left = 0;
                        } else if i == len - 1 {
                            rect.right = 0;
                        }
                    }
                }
            }
        }
    }
    fn enforce_no_resize(&mut self) {
        for rect in self.resize_dimensions.iter_mut().flatten() {
            rect.left = 0;
            rect.right = 0;
            rect.top = 0;
            rect.bottom = 0;
        }
    }
}

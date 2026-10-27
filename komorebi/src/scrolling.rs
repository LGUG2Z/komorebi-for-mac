//! The Scrolling layout of a workspace, which is laid out by [`komorebi_scrolling`] and not by the
//! layouts in `komorebi_layouts`.
//!
//! The other layouts are stateless: where a container goes is a function of the list of
//! containers. A strip of columns that a viewport scrolls over is not, because where the viewport
//! is and how wide each column has been made are facts that cannot be worked out from the list. So
//! a workspace in the Scrolling layout keeps a [`ScrollingSpace`] next to its containers, which is
//! told after every change what the containers are and which one has focus, and which answers with
//! where they all go.
//!
//! The list of containers stays what is true. Every layout change starts from it, and the space is
//! made to match it, so that nothing that changes the list has to know the space exists.

use crate::core::DefaultLayout;
use crate::core::Layout;
use crate::core::LayoutOptions;
use crate::core::OperationDirection;
use crate::core::Rect;
use crate::core::Sizing;
use crate::core::WindowHidingPosition;
use crate::core_graphics::CoreGraphicsApi;
use crate::is_visible_on_another_display;
use crate::workspace::Workspace;
use crate::workspace::clamp_to_work_area;
use color_eyre::eyre;
use komorebi_layouts::scrolling;
use komorebi_layouts::scrolling::MINIMUM_COLUMN_WIDTH;
use komorebi_layouts::scrolling::ScrollingPlacement;
use komorebi_layouts::scrolling::model::ColumnWidth;
use komorebi_layouts::scrolling::model::Options;
use komorebi_layouts::scrolling::model::Viewport;
use komorebi_layouts::scrolling::model::Visibility;
use objc2_core_foundation::CGRect;

/// What the layout options of a workspace come to for the model.
pub(crate) fn scrolling_options(layout_options: Option<LayoutOptions>) -> Options {
    scrolling::scrolling_options(layout_options)
}

impl Workspace {
    /// Whether the workspace is laid out as a strip of columns.
    pub(crate) fn is_scrolling(&self) -> bool {
        matches!(self.layout, Layout::Default(DefaultLayout::Scrolling))
    }

    /// Make the space match the containers, and the focus, as they are now.
    fn sync_scrolling(&mut self, viewport: Viewport) {
        let options = scrolling_options(self.effective_layout_options());
        let ids: Vec<String> = self
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect();
        let focused = self
            .focused_container()
            .map(|container| container.id.clone());

        scrolling::sync(
            &mut self.scrolling,
            options,
            viewport,
            &ids,
            focused.as_ref(),
        );
    }

    /// Make the space match the containers and the focus as they are now, for a space that is
    /// there to be made to match.
    ///
    /// This is for a change that is about to be made to the containers, and that the space is
    /// going to be told about directly. The space has to know what there is before it is told
    /// what goes.
    pub(crate) fn sync_scrolling_containers(&mut self) {
        let ids: Vec<String> = self
            .containers()
            .iter()
            .map(|container| container.id.clone())
            .collect();
        let focused = self
            .focused_container()
            .map(|container| container.id.clone());

        if let Some(space) = self.scrolling.as_mut() {
            space.reconcile(&ids, focused.as_ref());
        }
    }

    /// Give focus to what the strip says should have it after the container `removed` has gone,
    /// and tell whether the strip had anything to say.
    ///
    /// Focus goes back to the column that the closed one was opened next to, if nothing has
    /// moved focus since, and otherwise to the column that takes its place on the right. Focus that
    /// is not on the closed column stays where it is. This is not what the other layouts do, which
    /// always move focus to the left.
    pub(crate) fn focus_after_scrolling_removal(&mut self, removed: &str) -> bool {
        let Some(space) = self.scrolling.as_mut() else {
            return false;
        };

        if !space.remove_column(&removed.to_string()) {
            return false;
        }

        // a container is a tile, and the strip says where in reading order the one that has focus
        // is, which is the index of the container
        if let Some(idx) = space.active_tile_index() {
            self.focus_container(idx);
        }

        true
    }

    /// Where every container goes in `work_area`, from left to right, with `container_padding`
    /// taken off every side of each.
    pub(crate) fn layout_scrolling(
        &mut self,
        work_area: &Rect,
        container_padding: i32,
    ) -> Vec<ScrollingPlacement> {
        self.sync_scrolling(scrolling::viewport_of(work_area));

        let Some(space) = &self.scrolling else {
            return vec![];
        };

        scrolling::place(space, work_area, container_padding)
    }

    /// Lay the workspace out as a strip of columns and put every window where it goes.
    ///
    /// * `work_area` is the work area of the monitor, and `adjusted_work_area` is what is left of it
    ///   once offsets and padding are taken out.
    pub(crate) fn arrange_scrolling(
        &mut self,
        work_area: &Rect,
        adjusted_work_area: &Rect,
        container_padding: i32,
        border_offset: i32,
        border_width: i32,
    ) -> eyre::Result<()> {
        let placements = self.layout_scrolling(adjusted_work_area, container_padding);
        let screen = CoreGraphicsApi::display_bounds_for_window_rect(CGRect::from(*work_area));

        let mut latest_layout = Vec::with_capacity(placements.len());

        for (container, placement) in self.containers_mut().iter_mut().zip(&placements) {
            let mut rect = placement.rect;
            rect.add_padding(border_offset);
            rect.add_padding(border_width);
            latest_layout.push(rect);

            for window in container.windows_mut() {
                let result = match placement.visibility {
                    Visibility::Visible => window.set_position(&rect),
                    Visibility::Clipped => {
                        // part of it is in view and the rest is past the edge of the screen,
                        // which is all right unless there is another display past that edge,
                        // where the rest would be seen
                        let mut target = rect;
                        if overhangs_another_display(&rect, screen) {
                            clamp_to_work_area(&mut target, adjusted_work_area);
                        }

                        window.set_position(&target)
                    }
                    Visibility::OffLeft => window.hide_scrolling(WindowHidingPosition::BottomLeft),
                    Visibility::OffRight => {
                        window.hide_scrolling(WindowHidingPosition::BottomRight)
                    }
                };

                if let Err(error) = result {
                    tracing::warn!("failed to place window for scrolling layout: {error}");
                }
            }
        }

        self.latest_layout = latest_layout;

        Ok(())
    }

    /// Make the focused column wider or narrower. Only the horizontal edges mean anything, since
    /// a column is as tall as the viewport.
    ///
    /// The layouts that are made of splits move one edge of a container by half of `delta`, and
    /// callers that want to move both, or to follow a mouse, rely on that, so a column changes
    /// width by half of `delta` per edge too.
    pub(crate) fn resize_scrolling_column(
        &mut self,
        direction: OperationDirection,
        sizing: Sizing,
        delta: i32,
    ) {
        if !matches!(
            direction,
            OperationDirection::Left | OperationDirection::Right
        ) {
            return;
        }

        let Some(id) = self
            .focused_container()
            .map(|container| container.id.clone())
        else {
            return;
        };

        let Some(space) = self.scrolling.as_mut() else {
            return;
        };

        let Some(width) = space.column_width(&id) else {
            return;
        };

        let change = f64::from(delta) / 2.0;
        let width = match sizing {
            Sizing::Increase => width + change,
            Sizing::Decrease => width - change,
        };

        let width = (width.round() as i32).max(MINIMUM_COLUMN_WIDTH);
        space.set_width(&id, ColumnWidth::Fixed(width));
    }
}

/// Whether a window with this rectangle would be seen on a display other than the one that
/// `screen` is the bounds of.
///
/// If the display is not known, it is safer to say that it would.
fn overhangs_another_display(rect: &Rect, screen: Option<CGRect>) -> bool {
    match screen {
        Some(screen) => is_visible_on_another_display(CGRect::from(*rect), screen),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::Container;
    use crate::core::ScrollingLayoutOptions;
    use crate::window::Window;
    use komorebi_layouts::scrolling::ScrollingCentering;
    use komorebi_layouts::scrolling::model::CenterFocusedColumn;

    const AREA: Rect = Rect {
        left: 100,
        top: 50,
        right: 1200,
        bottom: 800,
    };

    fn options(
        columns: usize,
        center_focused_column: Option<ScrollingCentering>,
    ) -> Option<LayoutOptions> {
        Some(LayoutOptions {
            scrolling: Some(ScrollingLayoutOptions {
                columns,
                center_focused_column,
            }),
            grid: None,
            column_ratios: None,
            row_ratios: None,
        })
    }

    fn workspace(columns: usize, containers: usize) -> Workspace {
        let mut workspace = Workspace {
            layout: Layout::Default(DefaultLayout::Scrolling),
            layout_options: options(columns, None),
            ..Default::default()
        };

        for _ in 0..containers {
            workspace.add_container_to_back(Container::default());
        }

        workspace
    }

    /// Open a container the way a new window does: to the right of the focused one, and in focus.
    fn open(workspace: &mut Workspace) {
        let at = if workspace.containers().is_empty() {
            0
        } else {
            workspace.focused_container_idx() + 1
        };
        workspace.insert_container_at_idx(at, Container::default());
    }

    fn widths(placements: &[ScrollingPlacement]) -> Vec<i32> {
        placements
            .iter()
            .map(|placement| placement.rect.right)
            .collect()
    }

    #[test]
    fn a_column_is_a_share_of_the_viewport_as_wide_as_the_columns_option_says() {
        let options = scrolling_options(options(4, None));
        assert_eq!(options.default_width, ColumnWidth::Proportion(0.25));

        // and three if it says nothing at all
        let options = scrolling_options(None);
        assert_eq!(options.default_width, ColumnWidth::Proportion(1.0 / 3.0));

        // a count of nought cannot be a share of anything
        let options = scrolling_options(options_for(0));
        assert_eq!(options.default_width, ColumnWidth::Proportion(1.0));
    }

    fn options_for(columns: usize) -> Option<LayoutOptions> {
        options(columns, None)
    }

    #[test]
    fn centring_the_focused_column_is_what_the_layout_option_says() {
        for (given, expected) in [
            (None, CenterFocusedColumn::Never),
            (Some(ScrollingCentering::Never), CenterFocusedColumn::Never),
            (
                Some(ScrollingCentering::Always),
                CenterFocusedColumn::Always,
            ),
            (
                Some(ScrollingCentering::OnOverflow),
                CenterFocusedColumn::OnOverflow,
            ),
        ] {
            assert_eq!(
                scrolling_options(options(3, given)).center_focused_column,
                expected
            );
        }
    }

    #[test]
    fn containers_are_laid_out_inside_the_work_area_with_their_padding() {
        let mut workspace = workspace(4, 3);

        let placements = workspace.layout_scrolling(&AREA, 10);

        // 1200 / 4 = 300 a column, less 10 off every side
        assert_eq!(widths(&placements), [280, 280, 280]);
        assert_eq!(placements[0].rect.left, 100 + 10);
        assert_eq!(placements[1].rect.left, 100 + 300 + 10);
        assert_eq!(placements[0].rect.top, 50 + 10);
        assert_eq!(placements[0].rect.bottom, 800 - 20);
    }

    #[test]
    fn a_lone_container_is_not_stretched_to_fill_the_work_area() {
        let mut workspace = workspace(3, 1);

        let placements = workspace.layout_scrolling(&AREA, 0);

        assert_eq!(widths(&placements), [400]);
    }

    #[test]
    fn opening_a_window_does_not_resize_the_others() {
        let mut workspace = workspace(3, 0);

        for count in 1..=7 {
            open(&mut workspace);
            let placements = workspace.layout_scrolling(&AREA, 0);

            assert_eq!(placements.len(), count);
            assert_eq!(widths(&placements), vec![400; count]);
        }
    }

    #[test]
    fn the_view_stays_put_while_focus_moves_within_it() {
        let mut workspace = workspace(3, 6);
        workspace.focus_container(5);
        let initial = workspace.layout_scrolling(&AREA, 0);
        assert_eq!(visible(&initial), [3, 4, 5]);

        // moving focus left across columns that are already in view must not scroll the strip
        for idx in [4, 3] {
            workspace.focus_container(idx);
            assert_eq!(
                workspace.layout_scrolling(&AREA, 0),
                initial,
                "column {idx}"
            );
        }

        // leaving the view on the left scrolls just far enough to bring the column in
        workspace.focus_container(2);
        let scrolled = workspace.layout_scrolling(&AREA, 0);
        assert_eq!(visible(&scrolled), [2, 3, 4]);

        // and moving right within the new view leaves it where it is
        for idx in [3, 4] {
            workspace.focus_container(idx);
            assert_eq!(
                workspace.layout_scrolling(&AREA, 0),
                scrolled,
                "column {idx}"
            );
        }
    }

    fn visible(placements: &[ScrollingPlacement]) -> Vec<usize> {
        placements
            .iter()
            .enumerate()
            .filter(|(_, placement)| placement.visibility == Visibility::Visible)
            .map(|(idx, _)| idx)
            .collect()
    }

    #[test]
    fn a_container_that_scrolls_out_of_view_says_which_side_it_went_by() {
        let mut workspace = workspace(3, 6);
        // the strip scrolls to the end, and then focus moves back to where the last three begin
        workspace.focus_container(5);
        workspace.layout_scrolling(&AREA, 0);
        workspace.focus_container(3);

        let placements = workspace.layout_scrolling(&AREA, 0);

        let visibility: Vec<_> = placements
            .iter()
            .map(|placement| placement.visibility)
            .collect();
        assert_eq!(
            visibility,
            [
                Visibility::OffLeft,
                Visibility::OffLeft,
                Visibility::OffLeft,
                Visibility::Visible,
                Visibility::Visible,
                Visibility::Visible,
            ]
        );

        // a column that is out of view keeps the rectangle it has in the strip
        assert_eq!(placements[2].rect.left, 100 - 400);
    }

    #[test]
    fn a_column_that_was_made_wider_stays_wider_when_others_come_and_go() {
        let mut workspace = workspace(3, 3);
        workspace.layout_scrolling(&AREA, 0);

        workspace.focus_container(1);
        workspace.resize_scrolling_column(OperationDirection::Right, Sizing::Increase, 200);
        let placements = workspace.layout_scrolling(&AREA, 0);
        assert_eq!(widths(&placements), [400, 500, 400]);

        // a window opens to the right of the focused column, and moving focus does not change a width
        open(&mut workspace);
        workspace.focus_container(0);
        let placements = workspace.layout_scrolling(&AREA, 0);
        assert_eq!(widths(&placements), [400, 500, 400, 400]);
    }

    #[test]
    fn resizing_changes_the_width_by_half_of_the_delta_and_never_below_the_minimum() {
        let mut workspace = workspace(3, 2);
        workspace.layout_scrolling(&AREA, 0);

        workspace.resize_scrolling_column(OperationDirection::Left, Sizing::Increase, 100);
        assert_eq!(widths(&workspace.layout_scrolling(&AREA, 0))[1], 450);

        workspace.resize_scrolling_column(OperationDirection::Right, Sizing::Decrease, 100);
        assert_eq!(widths(&workspace.layout_scrolling(&AREA, 0))[1], 400);

        workspace.resize_scrolling_column(OperationDirection::Right, Sizing::Decrease, 10_000);
        assert_eq!(
            widths(&workspace.layout_scrolling(&AREA, 0))[1],
            MINIMUM_COLUMN_WIDTH
        );
    }

    #[test]
    fn a_column_is_as_tall_as_the_work_area_so_resizing_its_height_does_nothing() {
        let mut workspace = workspace(3, 2);
        let before = workspace.layout_scrolling(&AREA, 0);

        workspace.resize_scrolling_column(OperationDirection::Up, Sizing::Increase, 100);
        workspace.resize_scrolling_column(OperationDirection::Down, Sizing::Decrease, 100);

        assert_eq!(workspace.layout_scrolling(&AREA, 0), before);
    }

    #[test]
    fn resizing_before_there_is_a_layout_does_nothing() {
        // the space is made by the first layout, and there has not been one
        let mut workspace = workspace(3, 2);

        workspace.resize_scrolling_column(OperationDirection::Right, Sizing::Increase, 100);

        assert!(workspace.scrolling.is_none());
    }

    /// A workspace with a window in every container, which is how closing a window finds a container.
    fn workspace_of_windows(columns: usize, count: u32) -> Workspace {
        let mut workspace = Workspace {
            layout: Layout::Default(DefaultLayout::Scrolling),
            layout_options: options(columns, None),
            ..Default::default()
        };

        for id in 0..count {
            workspace
                .new_container_for_window(&Window::from(id))
                .unwrap();
        }

        workspace
    }

    fn focused_window(workspace: &Workspace) -> u32 {
        workspace
            .focused_container()
            .and_then(|container| container.focused_window())
            .map(|window| window.id)
            .unwrap()
    }

    #[test]
    fn closing_a_window_that_was_just_opened_goes_back_to_the_one_before_it() {
        let mut workspace = workspace_of_windows(3, 8);
        workspace.focus_container(3);
        workspace.layout_scrolling(&AREA, 0);
        // window 5 is the last of the three that are in view, and 6 and 7 are beyond it
        workspace.focus_container(5);
        let before = workspace.layout_scrolling(&AREA, 0);

        workspace
            .new_container_for_window(&Window::from(100))
            .unwrap();
        let opened = workspace.layout_scrolling(&AREA, 0);
        assert_ne!(opened, before, "opening it scrolled the strip");

        workspace.remove_window(100).unwrap();

        assert_eq!(focused_window(&workspace), 5);
        assert_eq!(workspace.layout_scrolling(&AREA, 0), before);
    }

    #[test]
    fn once_focus_has_moved_closing_a_window_goes_to_the_one_that_takes_its_place() {
        let mut workspace = workspace_of_windows(3, 8);
        workspace.focus_container(3);
        workspace.layout_scrolling(&AREA, 0);
        workspace.focus_container(5);
        workspace.layout_scrolling(&AREA, 0);

        workspace
            .new_container_for_window(&Window::from(100))
            .unwrap();
        workspace.layout_scrolling(&AREA, 0);
        workspace.focus_container(5);
        workspace.layout_scrolling(&AREA, 0);
        workspace.focus_container(6);
        workspace.layout_scrolling(&AREA, 0);

        workspace.remove_window(100).unwrap();

        // window 6 was to the right of the closed window, and the other layouts would have gone left
        assert_eq!(focused_window(&workspace), 6);
    }

    #[test]
    fn closing_the_last_window_goes_to_the_one_before_it() {
        let mut workspace = workspace_of_windows(3, 5);
        workspace.layout_scrolling(&AREA, 0);
        assert_eq!(focused_window(&workspace), 4);

        workspace.remove_window(4).unwrap();

        assert_eq!(focused_window(&workspace), 3);
    }

    #[test]
    fn closing_a_window_that_does_not_have_focus_leaves_focus_where_it_is() {
        let mut workspace = workspace_of_windows(3, 6);
        workspace.focus_container(2);
        workspace.layout_scrolling(&AREA, 0);

        // to the right of the focused one, and then to the left of it
        workspace.remove_window(5).unwrap();
        assert_eq!(focused_window(&workspace), 2);

        workspace.remove_window(0).unwrap();
        assert_eq!(focused_window(&workspace), 2);
        assert_eq!(workspace.focused_container_idx(), 1);
    }

    #[test]
    fn before_there_is_a_strip_closing_a_window_moves_focus_left_like_the_other_layouts() {
        let mut workspace = workspace_of_windows(3, 4);
        workspace.focus_container(2);
        assert!(workspace.scrolling.is_none());

        workspace.remove_window(2).unwrap();

        assert_eq!(focused_window(&workspace), 1);
    }

    #[test]
    fn changing_the_columns_option_changes_the_width_of_columns_that_have_not_been_resized() {
        let mut workspace = workspace(3, 3);
        assert_eq!(
            widths(&workspace.layout_scrolling(&AREA, 0)),
            [400, 400, 400]
        );

        workspace.set_scrolling_columns(4);

        assert_eq!(
            widths(&workspace.layout_scrolling(&AREA, 0)),
            [300, 300, 300]
        );
    }
}

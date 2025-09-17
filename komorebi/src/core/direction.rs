use crate::core::default_layout::DefaultLayout;
use crate::core::operation_direction::OperationDirection;

pub trait Direction {
    fn index_in_direction(
        &self,
        op_direction: OperationDirection,
        idx: usize,
        count: usize,
    ) -> Option<usize>;

    fn is_valid_direction(
        &self,
        op_direction: OperationDirection,
        idx: usize,
        count: usize,
    ) -> bool;
    fn up_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize;
    fn down_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize;
    fn left_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize;
    fn right_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize;
}

impl Direction for DefaultLayout {
    fn index_in_direction(
        &self,
        op_direction: OperationDirection,
        idx: usize,
        count: usize,
    ) -> Option<usize> {
        match op_direction {
            OperationDirection::Left => {
                if self.is_valid_direction(op_direction, idx, count) {
                    Option::from(self.left_index(Some(op_direction), idx, Some(count)))
                } else {
                    None
                }
            }
            OperationDirection::Right => {
                if self.is_valid_direction(op_direction, idx, count) {
                    Option::from(self.right_index(Some(op_direction), idx, Some(count)))
                } else {
                    None
                }
            }
            OperationDirection::Up => {
                if self.is_valid_direction(op_direction, idx, count) {
                    Option::from(self.up_index(Some(op_direction), idx, Some(count)))
                } else {
                    None
                }
            }
            OperationDirection::Down => {
                if self.is_valid_direction(op_direction, idx, count) {
                    Option::from(self.down_index(Some(op_direction), idx, Some(count)))
                } else {
                    None
                }
            }
        }
    }

    fn is_valid_direction(
        &self,
        op_direction: OperationDirection,
        idx: usize,
        count: usize,
    ) -> bool {
        if count < 2 {
            return false;
        }

        match op_direction {
            OperationDirection::Up => match self {
                Self::BSP => idx != 0 && idx != 1,
                Self::Columns => false,
                Self::Rows | Self::HorizontalStack => idx != 0,
                Self::VerticalStack | Self::RightMainVerticalStack => idx != 0 && idx != 1,
                Self::UltrawideVerticalStack => idx > 2,
                Self::Grid => !is_grid_edge(op_direction, idx, count),
                Self::Scrolling => false,
            },
            OperationDirection::Down => match self {
                Self::BSP => idx != count - 1 && idx % 2 != 0,
                Self::Columns => false,
                Self::Rows => idx != count - 1,
                Self::VerticalStack | Self::RightMainVerticalStack => idx != 0 && idx != count - 1,
                Self::HorizontalStack => idx == 0,
                Self::UltrawideVerticalStack => idx > 1 && idx != count - 1,
                Self::Grid => !is_grid_edge(op_direction, idx, count),
                Self::Scrolling => false,
            },
            OperationDirection::Left => match self {
                Self::BSP => idx != 0,
                Self::Columns | Self::VerticalStack => idx != 0,
                Self::RightMainVerticalStack => idx == 0,
                Self::Rows => false,
                Self::HorizontalStack => idx != 0 && idx != 1,
                Self::UltrawideVerticalStack => idx != 1,
                Self::Grid => !is_grid_edge(op_direction, idx, count),
                Self::Scrolling => idx != 0,
            },
            OperationDirection::Right => match self {
                Self::BSP => idx % 2 == 0 && idx != count - 1,
                Self::Columns => idx != count - 1,
                Self::Rows => false,
                Self::VerticalStack => idx == 0,
                Self::RightMainVerticalStack => idx != 0,
                Self::HorizontalStack => idx != 0 && idx != count - 1,
                Self::UltrawideVerticalStack => match count {
                    2 => idx != 0,
                    _ => idx < 2,
                },
                Self::Grid => !is_grid_edge(op_direction, idx, count),
                Self::Scrolling => idx != count - 1,
            },
        }
    }

    fn up_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize {
        match self {
            Self::BSP => {
                if idx % 2 == 0 {
                    idx - 1
                } else {
                    idx - 2
                }
            }
            Self::Columns => unreachable!(),
            Self::Rows
            | Self::VerticalStack
            | Self::UltrawideVerticalStack
            | Self::RightMainVerticalStack => idx - 1,
            Self::HorizontalStack => 0,
            Self::Grid => grid_neighbor(op_direction, idx, count),
            Self::Scrolling => unreachable!(),
        }
    }

    fn down_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize {
        match self {
            Self::BSP
            | Self::Rows
            | Self::VerticalStack
            | Self::UltrawideVerticalStack
            | Self::RightMainVerticalStack => idx + 1,
            Self::Columns => unreachable!(),
            Self::HorizontalStack => 1,
            Self::Grid => grid_neighbor(op_direction, idx, count),
            Self::Scrolling => unreachable!(),
        }
    }

    fn left_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize {
        match self {
            Self::BSP => {
                if idx % 2 == 0 {
                    idx - 2
                } else {
                    idx - 1
                }
            }
            Self::Columns | Self::HorizontalStack => idx - 1,
            Self::Rows => unreachable!(),
            Self::VerticalStack => 0,
            Self::RightMainVerticalStack => 1,
            Self::UltrawideVerticalStack => match idx {
                0 => 1,
                1 => unreachable!(),
                _ => 0,
            },
            Self::Grid => grid_neighbor(op_direction, idx, count),
            Self::Scrolling => idx - 1,
        }
    }

    fn right_index(
        &self,
        op_direction: Option<OperationDirection>,
        idx: usize,
        count: Option<usize>,
    ) -> usize {
        match self {
            Self::BSP | Self::Columns | Self::HorizontalStack => idx + 1,
            Self::Rows => unreachable!(),
            Self::VerticalStack => 1,
            Self::RightMainVerticalStack => 0,
            Self::UltrawideVerticalStack => match idx {
                1 => 0,
                0 => 2,
                _ => unreachable!(),
            },
            Self::Grid => grid_neighbor(op_direction, idx, count),
            Self::Scrolling => idx + 1,
        }
    }
}

struct GridItem {
    state: GridItemState,
    row: usize,
    num_rows: usize,
    touching_edges: GridTouchingEdges,
}

enum GridItemState {
    Valid,
    Invalid,
}

#[allow(clippy::struct_excessive_bools)]
struct GridTouchingEdges {
    left: bool,
    right: bool,
    up: bool,
    down: bool,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn get_grid_item(idx: usize, count: usize) -> GridItem {
    let num_cols = (count as f32).sqrt().ceil() as usize;
    let mut iter = 0;

    for col in 0..num_cols {
        let remaining_windows = count - iter;
        let remaining_columns = num_cols - col;
        let num_rows_in_this_col = remaining_windows / remaining_columns;

        for row in 0..num_rows_in_this_col {
            if iter == idx {
                return GridItem {
                    state: GridItemState::Valid,
                    row: row + 1,
                    num_rows: num_rows_in_this_col,
                    touching_edges: GridTouchingEdges {
                        left: col == 0,
                        right: col == num_cols - 1,
                        up: row == 0,
                        down: row == num_rows_in_this_col - 1,
                    },
                };
            }

            iter += 1;
        }
    }

    GridItem {
        state: GridItemState::Invalid,
        row: 0,
        num_rows: 0,
        touching_edges: GridTouchingEdges {
            left: true,
            right: true,
            up: true,
            down: true,
        },
    }
}

fn is_grid_edge(op_direction: OperationDirection, idx: usize, count: usize) -> bool {
    let item = get_grid_item(idx, count);

    match item.state {
        GridItemState::Invalid => false,
        GridItemState::Valid => match op_direction {
            OperationDirection::Left => item.touching_edges.left,
            OperationDirection::Right => item.touching_edges.right,
            OperationDirection::Up => item.touching_edges.up,
            OperationDirection::Down => item.touching_edges.down,
        },
    }
}

fn grid_neighbor(
    op_direction: Option<OperationDirection>,
    idx: usize,
    count: Option<usize>,
) -> usize {
    let Some(op_direction) = op_direction else {
        return 0;
    };

    let Some(count) = count else {
        return 0;
    };

    let item = get_grid_item(idx, count);

    match op_direction {
        OperationDirection::Left => {
            let item_from_prev_col = get_grid_item(idx - item.row, count);

            if item.touching_edges.up && item.num_rows != item_from_prev_col.num_rows {
                return idx - (item.num_rows - 1);
            }

            if item.num_rows != item_from_prev_col.num_rows && !item.touching_edges.down {
                return idx - (item.num_rows - 1);
            }

            idx - item.num_rows
        }
        OperationDirection::Right => idx + item.num_rows,
        OperationDirection::Up => idx - 1,
        OperationDirection::Down => idx + 1,
    }
}

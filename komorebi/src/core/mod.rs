use serde::Deserialize;
use serde::Serialize;

pub mod arrangement;
pub mod default_layout;
pub mod direction;
pub mod layout;
pub mod operation_direction;
pub mod rect;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Sizing {
    Increase,
    Decrease,
}

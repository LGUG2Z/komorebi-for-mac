use crate::core::arrangement::Arrangement;
use crate::core::default_layout::DefaultLayout;
use crate::core::direction::Direction;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Layout {
    Default(DefaultLayout),
}

impl Layout {
    #[must_use]
    pub fn as_boxed_direction(&self) -> Box<dyn Direction> {
        match self {
            Layout::Default(layout) => Box::new(*layout),
        }
    }

    #[must_use]
    pub fn as_boxed_arrangement(&self) -> Box<dyn Arrangement> {
        match self {
            Layout::Default(layout) => Box::new(*layout),
        }
    }
}

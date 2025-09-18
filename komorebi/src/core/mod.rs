use crate::core::cycle_direction::CycleDirection;
use crate::core::default_layout::DefaultLayout;
use crate::core::operation_direction::OperationDirection;
use color_eyre::eyre;
use serde::Deserialize;
use serde::Serialize;
use std::str::FromStr;
use strum::Display;

pub mod arrangement;
pub mod cycle_direction;
pub mod default_layout;
pub mod direction;
pub mod layout;
pub mod operation_direction;
pub mod pathext;
pub mod rect;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum Sizing {
    Increase,
    Decrease,
}

#[serde_with::serde_as]
#[derive(Clone, Debug, Serialize, Deserialize, Display)]
#[serde(tag = "type", content = "content")]
pub enum SocketMessage {
    // Window / Container Commands
    FocusWindow(OperationDirection),
    MoveWindow(OperationDirection),
    StackWindow(OperationDirection),
    CycleStack(CycleDirection),
    UnstackWindow,
    ChangeLayout(DefaultLayout),
    TogglePause,
    FocusWorkspaceNumber(usize),
    MoveContainerToWorkspaceNumber(usize),
    SendContainerToWorkspaceNumber(usize),
}

impl SocketMessage {
    pub fn as_bytes(&self) -> eyre::Result<Vec<u8>> {
        Ok(serde_json::to_string(self)?.as_bytes().to_vec())
    }
}

impl FromStr for SocketMessage {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

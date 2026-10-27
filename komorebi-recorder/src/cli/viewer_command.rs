//! `view`: the arguments of the viewer window.

use clap::Args;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ViewArgs {
    /// The recording's folder [default: choose one in the window]
    pub session: Option<PathBuf>,
}

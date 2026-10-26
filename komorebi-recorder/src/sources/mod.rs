//! The non-video capture sources. Each one writes its own raw files into the session directory
//! and never rewrites them afterwards.

pub mod komorebi_events;
pub mod komorebi_log;
pub mod komorebi_version;
pub mod managed_apps;
pub mod oslog;

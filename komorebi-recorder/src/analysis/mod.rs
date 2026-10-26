//! Understanding a recorded session without a window.
//!
//! Everything here reads a session directory and produces plain serializable values, so the same
//! answers serve the agent-facing command line and, later, the viewer. This module holds the
//! parts every command shares: the JSON envelope and exit codes ([`envelope`]), how a point in time
//! is named ([`timespec`], [`ids`]), the guard that keeps file access inside the session
//! ([`guard`]) and the budget that keeps an answer from growing without bound ([`budget`]).

pub mod budget;
pub mod envelope;
pub mod guard;
pub mod ids;
pub mod timespec;

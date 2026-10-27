//! Playing the recording: a clock that can run forwards or backwards at any speed, and an engine
//! that keeps the frame of every display in step with it.
//!
//! The clock ([`transport`]) is plain arithmetic with no window and no decoder, so it is tested to
//! the millisecond. The engine ([`engine`]) owns the decoding on its own thread, since AVFoundation's
//! objects cannot be shared, and publishes the frame to show for each display. Whatever draws (the
//! viewer) asks the engine what time it is and what to show, and never decodes itself.

pub mod engine;
pub mod transport;

pub use engine::DisplayView;
pub use engine::Frame;
pub use engine::Playback;
pub use transport::Transport;

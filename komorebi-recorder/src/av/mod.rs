//! Decoding the recorded video with AVFoundation.
//!
//! The recorder writes each display's video as a fragmented mp4. Reading it back means asking
//! AVFoundation for the frames between two times, decoded to BGRA at a size of the caller's
//! choosing (the decoder scales, so a contact sheet never decodes more pixels than it shows).
//!
//! AVFoundation's objects are not `Send` and some calls raise an Objective-C exception instead of
//! returning an error when used in the wrong order, which would abort the process. The types here
//! make the wrong order impossible to write: the only way to a [`Reader`] is
//! [`Asset::start`], which configures everything before reading begins.

mod reader;

pub use reader::Asset;
pub use reader::Frame;
pub use reader::Reader;
pub use reader::Scale;

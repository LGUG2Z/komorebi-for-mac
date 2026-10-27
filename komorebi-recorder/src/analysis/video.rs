//! The recorded video, addressed by session time and display.
//!
//! A display's video is one file per segment (a display that was unplugged or slept and came
//! back starts a new one), and a file's own time starts at its first frame. This maps a moment of
//! session video time on a display to the file and file time that has it, decodes frames, and says
//! when there is no video to show (the display was absent).

use super::guard::SessionPath;
use crate::av::Asset;
use crate::av::Frame;
use crate::av::Scale;
use crate::session::DisplayEntry;
use crate::session::Manifest;
use color_eyre::eyre;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// A decoded frame and when it was actually on screen.
pub struct DecodedFrame {
    pub display: u32,
    /// The segment of the display's video it came from.
    pub segment: u32,
    /// When the frame was shown, in session video time. Never later than what was asked for.
    pub shown_s: f64,
    pub frame: Frame,
}

/// The video of a session. Not `Send`: decoding objects belong to the thread that made them.
pub struct SessionVideo<'a> {
    manifest: &'a Manifest,
    guard: &'a SessionPath,
    assets: RefCell<HashMap<PathBuf, Rc<Asset>>>,
}

impl<'a> SessionVideo<'a> {
    #[must_use]
    pub fn new(manifest: &'a Manifest, guard: &'a SessionPath) -> Self {
        Self {
            manifest,
            guard,
            assets: RefCell::new(HashMap::new()),
        }
    }

    /// The displays that have any video.
    #[must_use]
    pub fn displays(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .manifest
            .displays
            .iter()
            .filter(|entry| entry.result.frames > 0)
            .map(|entry| entry.result.display.id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn asset(&self, entry: &DisplayEntry) -> eyre::Result<Rc<Asset>> {
        let path = self.guard.resolve(&entry.result.file)?;
        if let Some(found) = self.assets.borrow().get(&path) {
            return Ok(found.clone());
        }

        let asset = Rc::new(Asset::open(&path)?);
        self.assets.borrow_mut().insert(path, asset.clone());
        Ok(asset)
    }

    /// The frame of `display` on screen at session video time `video_s`, or `None` if the display
    /// had no video then.
    pub fn frame_at(
        &self,
        display: u32,
        video_s: f64,
        scale: Scale,
    ) -> eyre::Result<Option<DecodedFrame>> {
        let Some(entry) = self.manifest.segment_at(display, video_s) else {
            return Ok(None);
        };

        let asset = self.asset(entry)?;
        // the last moment of a file is the last frame, not past it
        let file_s = (video_s - entry.offset_s)
            .max(0.0)
            .min((asset.duration_s() - 0.002).max(0.0));
        let mut reader = asset.start(file_s, Some(file_s + 0.5), scale)?;
        Ok(reader.next_frame()?.map(|frame| DecodedFrame {
            display,
            segment: entry.result.segment,
            shown_s: entry.offset_s + frame.pts_s,
            frame,
        }))
    }

    /// Decode every frame of `display` from `from_s` to `to_s`, across segments, in order, calling
    /// `visit` with each until it returns `false`. Returns how many frames were visited.
    pub fn scan(
        &self,
        display: u32,
        from_s: f64,
        to_s: f64,
        scale: Scale,
        visit: &mut dyn FnMut(&DecodedFrame) -> bool,
    ) -> eyre::Result<usize> {
        let mut segments: Vec<&DisplayEntry> = self
            .manifest
            .displays
            .iter()
            .filter(|entry| entry.result.display.id == display && entry.result.frames > 0)
            .collect();
        segments.sort_by(|a, b| a.offset_s.total_cmp(&b.offset_s));
        let (_, session_end) = self.manifest.video_span();

        let mut visited = 0;
        for entry in segments {
            let end = entry.end_s.unwrap_or(session_end);
            if end < from_s || entry.offset_s > to_s {
                continue;
            }

            let asset = self.asset(entry)?;
            let start = (from_s - entry.offset_s).max(0.0);
            let stop = (to_s - entry.offset_s).min(asset.duration_s());
            let mut reader = asset.start(start, Some(stop), scale)?;
            while let Some(frame) = reader.next_frame()? {
                let shown_s = entry.offset_s + frame.pts_s;
                // the decoder starts at the frame on screen at `from_s`, which can be before it
                if shown_s + 1e-6 < from_s && shown_s + 0.5 < from_s {
                    continue;
                }
                if shown_s > to_s {
                    break;
                }

                visited += 1;
                let decoded = DecodedFrame {
                    display,
                    segment: entry.result.segment,
                    shown_s,
                    frame,
                };
                if !visit(&decoded) {
                    return Ok(visited);
                }
            }
        }

        Ok(visited)
    }
}

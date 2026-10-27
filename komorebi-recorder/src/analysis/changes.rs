//! When, and where on the screen, the picture changed.
//!
//! komorebi's events say when it thought something happened; the video says when the screen
//! actually did. Comparing consecutive frames finds the stretches where pixels changed, how much
//! of the screen they covered and the box around them, so an investigator can see whether a layout
//! change was instant or took half a second, whether a window jumped, and what else on screen was
//! going on. Where an event falls inside a stretch of change it is named.

use super::Analysis;
use super::envelope;
use super::video::DecodedFrame;
use super::video::SessionVideo;
use crate::av::Scale;
use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;

pub const SCHEMA_CHANGES: &str = "changes/1";

/// Frames are compared at this width: enough to see a window move, cheap enough to scan minutes.
const COMPARE_WIDTH: u32 = 240;

/// A pixel counts as changed if its brightness moved by more than this (out of 255).
const PIXEL_THRESHOLD: i32 = 24;

/// A frame is a change if at least this share of its pixels changed, at sensitivity 1.
const SHARE_THRESHOLD: f64 = 0.0005;

/// Frames of one change closer together than this are one change.
const MERGE_GAP_S: f64 = 0.15;

/// A display whose first frame arrives this late, or whose video stops this early, is not
/// reported as having a gap: frames come when the screen changes, not on a schedule.
const MIN_GAP_S: f64 = 0.5;

/// An event this close to a change is said to coincide with it.
const COINCIDE_S: f64 = 0.25;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ChangeSpan {
    pub display: u32,
    pub start_video_s: f64,
    pub end_video_s: f64,
    /// Frames in the span.
    pub frames: usize,
    /// The largest share of the screen that changed between two consecutive frames.
    pub peak_share: f64,
    /// The box around everything that changed, as fractions of the screen: `[x, y, width, height]`.
    pub bbox: [f64; 4],
    /// The same box in the display's own coordinates (global points), if the display's frame is
    /// known: `[x, y, width, height]`.
    pub bbox_points: Option<[f64; 4]>,
    /// Events received during the span or just around it.
    pub events: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Gap {
    pub display: u32,
    pub from_video_s: f64,
    pub to_video_s: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ChangesData {
    pub from_video_s: f64,
    pub to_video_s: f64,
    pub displays: Vec<u32>,
    pub frames_examined: usize,
    pub spans: Vec<ChangeSpan>,
    /// Stretches of the range where a display had no video at all (unplugged, asleep).
    pub gaps: Vec<Gap>,
}

/// The brightness of every pixel of a BGRA frame.
fn luma(frame: &DecodedFrame) -> Vec<u8> {
    frame
        .frame
        .bgra
        .chunks_exact(4)
        .map(|pixel| {
            // Rec. 601 weights in integers
            ((u32::from(pixel[2]) * 299 + u32::from(pixel[1]) * 587 + u32::from(pixel[0]) * 114)
                / 1000) as u8
        })
        .collect()
}

struct Open {
    start: f64,
    end: f64,
    frames: usize,
    peak: f64,
    bounds: (u32, u32, u32, u32),
}

impl Analysis {
    /// Find where the picture changed on each display in `from_s..=to_s`.
    pub fn changes(
        &self,
        video: &SessionVideo,
        displays: &[u32],
        from_s: f64,
        to_s: f64,
        sensitivity: f64,
    ) -> eyre::Result<ChangesData> {
        if sensitivity <= 0.0 {
            return Err(envelope::usage("--sensitivity must be above zero"));
        }

        let threshold = SHARE_THRESHOLD / sensitivity;
        let mut spans = Vec::new();
        let mut examined = 0;

        for display in displays {
            let frame_box = self
                .manifest
                .displays
                .iter()
                .find(|entry| entry.result.display.id == *display)
                .map(|entry| entry.result.display.frame);

            let mut previous: Option<(Vec<u8>, u32, u32)> = None;
            let mut open: Option<Open> = None;
            let mut finished: Vec<Open> = Vec::new();

            examined += video.scan(
                *display,
                from_s,
                to_s,
                Scale::Width(COMPARE_WIDTH),
                &mut |decoded| {
                    let now = luma(decoded);
                    let (width, height) = (decoded.frame.width, decoded.frame.height);

                    if let Some((before, before_width, before_height)) = &previous
                        && (*before_width, *before_height) == (width, height)
                    {
                        let (mut count, mut min_x, mut min_y, mut max_x, mut max_y) =
                            (0_usize, u32::MAX, u32::MAX, 0_u32, 0_u32);
                        for (index, (a, b)) in before.iter().zip(&now).enumerate() {
                            if (i32::from(*a) - i32::from(*b)).abs() > PIXEL_THRESHOLD {
                                let (x, y) = (index as u32 % width, index as u32 / width);
                                count += 1;
                                min_x = min_x.min(x);
                                max_x = max_x.max(x);
                                min_y = min_y.min(y);
                                max_y = max_y.max(y);
                            }
                        }

                        let share = count as f64 / now.len().max(1) as f64;
                        if share >= threshold {
                            match &mut open {
                                Some(span) if decoded.shown_s - span.end <= MERGE_GAP_S => {
                                    span.end = decoded.shown_s;
                                    span.frames += 1;
                                    span.peak = span.peak.max(share);
                                    span.bounds = (
                                        span.bounds.0.min(min_x),
                                        span.bounds.1.min(min_y),
                                        span.bounds.2.max(max_x),
                                        span.bounds.3.max(max_y),
                                    );
                                }
                                _ => {
                                    finished.extend(open.take());
                                    open = Some(Open {
                                        start: decoded.shown_s,
                                        end: decoded.shown_s,
                                        frames: 1,
                                        peak: share,
                                        bounds: (min_x, min_y, max_x, max_y),
                                    });
                                }
                            }
                        }
                    }

                    previous = Some((now, width, height));
                    true
                },
            )?;
            finished.extend(open.take());

            let dimensions = previous
                .as_ref()
                .map(|(_, width, height)| (*width, *height));
            for span in finished {
                let (width, height) = dimensions.unwrap_or((1, 1));
                let (w, h) = (f64::from(width), f64::from(height));
                let bbox = [
                    f64::from(span.bounds.0) / w,
                    f64::from(span.bounds.1) / h,
                    f64::from(span.bounds.2 - span.bounds.0 + 1) / w,
                    f64::from(span.bounds.3 - span.bounds.1 + 1) / h,
                ];
                let events = self
                    .events()
                    .iter()
                    .filter(|event| {
                        let at = self.video_s(event.recv_mach_ns);
                        at >= span.start - COINCIDE_S && at <= span.end + COINCIDE_S
                    })
                    .map(|event| event.seq)
                    .collect();

                spans.push(ChangeSpan {
                    display: *display,
                    start_video_s: span.start,
                    end_video_s: span.end,
                    frames: span.frames,
                    peak_share: span.peak,
                    bbox,
                    bbox_points: frame_box.map(|[x, y, frame_width, frame_height]| {
                        [
                            x + bbox[0] * frame_width,
                            y + bbox[1] * frame_height,
                            bbox[2] * frame_width,
                            bbox[3] * frame_height,
                        ]
                    }),
                    events,
                });
            }
        }

        spans.sort_by(|a, b| {
            a.start_video_s
                .total_cmp(&b.start_video_s)
                .then(a.display.cmp(&b.display))
        });

        Ok(ChangesData {
            from_video_s: from_s,
            to_video_s: to_s,
            displays: displays.to_vec(),
            frames_examined: examined,
            spans,
            gaps: self.video_gaps(displays, from_s, to_s),
        })
    }

    /// Stretches of `from_s..=to_s` in which a display has no video.
    #[must_use]
    pub fn video_gaps(&self, displays: &[u32], from_s: f64, to_s: f64) -> Vec<Gap> {
        let mut gaps = Vec::new();
        for display in displays {
            let mut covered: Vec<(f64, f64)> = self
                .manifest
                .displays
                .iter()
                .filter(|entry| entry.result.display.id == *display && entry.result.frames > 0)
                .map(|entry| {
                    (
                        entry.offset_s,
                        entry.end_s.unwrap_or_else(|| self.manifest.video_span().1),
                    )
                })
                .collect();
            covered.sort_by(|a, b| a.0.total_cmp(&b.0));

            let mut cursor = from_s;
            for (start, end) in covered {
                if start > cursor + MIN_GAP_S && cursor < to_s {
                    gaps.push(Gap {
                        display: *display,
                        from_video_s: cursor,
                        to_video_s: start.min(to_s),
                    });
                }
                cursor = cursor.max(end);
            }
            if cursor + MIN_GAP_S < to_s {
                gaps.push(Gap {
                    display: *display,
                    from_video_s: cursor,
                    to_video_s: to_s,
                });
            }
        }

        gaps
    }
}

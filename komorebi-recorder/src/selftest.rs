//! The sync self-test: prove that "video second N" really is "clock time N".
//!
//! A child process (so the recorder does not exclude it from the capture along with its own
//! windows) draws a large live clock plus a strip of 40 black/white cells that binary-encodes the
//! host clock in milliseconds at the moment each frame was painted. The parent records the screen
//! headlessly, then for several video timestamps decodes the strip out of the extracted frame and
//! compares it with the time the session's manifest predicts for that video second. Nothing is
//! read by eye or OCR.
//!
//! What the comparison includes beyond the recorder's own accuracy: the clock is painted a
//! little before the compositor shows it and SCK frames are rate-limited, so the visible clock
//! trails the video time by one to a few frame intervals. That is why the default tolerance is
//! 100 ms and the mean bias is reported separately.

use crate::capture::sck;
use crate::cli::SelftestArgs;
use crate::clock;
use crate::frames;
use crate::recorder;
use crate::recorder::Live;
use crate::recorder::OsLogOptions;
use crate::recorder::RecordOptions;
use crate::session::Calibration;
use crate::session::CalibrationSample;
use crate::session::Manifest;
use crate::session::Session;
use crate::timecode;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use eframe::egui;
use objc2_core_graphics::CGMainDisplayID;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

/// Number of cells in the strip, i.e. bits of the encoded millisecond clock.
const CELLS: usize = 40;
const CELL_WIDTH: f32 = 24.0;
const CELL_HEIGHT: f32 = 60.0;
/// Where the strip sits inside the clock window, in points.
const STRIP_X: f32 = 20.0;
const STRIP_Y: f32 = 110.0;
const WINDOW_SIZE: [f32; 2] = [1000.0, 260.0];
const WINDOW_POSITION: [f32; 2] = [80.0, 80.0];

const WRAP_MS: u64 = 1 << CELLS;

/// The cell colours (true = white) for a clock value in milliseconds, most significant first.
#[must_use]
pub fn encode_cells(ms: u64) -> [bool; CELLS] {
    let mut cells = [false; CELLS];
    for (index, cell) in cells.iter_mut().enumerate() {
        *cell = (ms >> (CELLS - 1 - index)) & 1 == 1;
    }
    cells
}

/// Recover the clock value from the average luminance of each cell.
#[must_use]
pub fn decode_cells(luminance: &[u8]) -> Option<u64> {
    if luminance.len() != CELLS {
        return None;
    }

    // a legible strip is overwhelmingly near-black or near-white; a mid-grey average means the
    // crop missed the strip
    let ambiguous = luminance
        .iter()
        .filter(|value| (64..192).contains(*value))
        .count();
    if ambiguous > 3 {
        return None;
    }

    Some(luminance.iter().fold(0_u64, |value, level| {
        (value << 1) | u64::from(*level >= 128)
    }))
}

/// `observed - predicted` in milliseconds, accounting for the strip's wrap-around.
#[must_use]
pub fn wrapped_difference_ms(observed: u64, predicted: u64) -> i64 {
    let wrapped = (observed.wrapping_sub(predicted)) % WRAP_MS;
    if wrapped >= WRAP_MS / 2 {
        wrapped as i64 - WRAP_MS as i64
    } else {
        wrapped as i64
    }
}

/// The ffmpeg `-vf` expression that crops the strip out of a display's video and averages each
/// cell down to one grey pixel.
#[must_use]
pub fn strip_filter(window_origin: (f64, f64), display_origin: (f64, f64), scale: f64) -> String {
    let x = ((window_origin.0 - display_origin.0) + f64::from(STRIP_X)) * scale;
    let y = ((window_origin.1 - display_origin.1) + f64::from(STRIP_Y)) * scale;
    let width = f64::from(CELL_WIDTH) * CELLS as f64 * scale;
    // sample the middle of the cells vertically
    let height = f64::from(CELL_HEIGHT) * 0.5 * scale;
    let y = y + f64::from(CELL_HEIGHT) * 0.25 * scale;

    format!(
        "crop={}:{}:{}:{},scale={CELLS}:1:flags=area",
        width.round() as i64,
        height.round().max(1.0) as i64,
        x.round() as i64,
        y.round() as i64,
    )
}

/// The child process: a borderless, untitled (so komorebi leaves it alone) window that paints
/// the clock as fast as it can.
pub fn run_clock() -> eyre::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("")
            .with_decorations(false)
            .with_position(WINDOW_POSITION)
            .with_inner_size(WINDOW_SIZE)
            .with_always_on_top()
            .with_active(false),
        ..Default::default()
    };

    eframe::run_native(
        "komorebi-recorder-clock",
        options,
        Box::new(|_| Ok(Box::new(ClockApp::default()))),
    )
    .map_err(|error| eyre::Report::msg(format!("the clock window failed: {error}")))
}

#[derive(Default)]
struct ClockApp {
    last_rect: Option<egui::Rect>,
    started: Option<Instant>,
}

impl eframe::App for ClockApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        let started = *self.started.get_or_insert_with(Instant::now);
        // never linger if the parent dies without killing us
        if started.elapsed() > Duration::from_secs(120) {
            std::process::exit(0);
        }

        // read the clock first: this is the moment the frame's content is decided
        let mach_ns = clock::mach_now_ns();
        let wall_us = clock::wall_now_us();
        let ms = (mach_ns / 1_000_000) % WRAP_MS;

        if let Some(rect) = context.input(|input| input.viewport().inner_rect)
            && self.last_rect != Some(rect)
        {
            self.last_rect = Some(rect);
            println!(
                "RECT {} {} {} {}",
                rect.min.x,
                rect.min.y,
                rect.width(),
                rect.height()
            );
            let _ = std::io::stdout().flush();
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(40)))
            .show(context, |ui| {
                let painter = ui.painter();
                painter.text(
                    egui::pos2(20.0, 14.0),
                    egui::Align2::LEFT_TOP,
                    format!("mach {mach_ns} ns"),
                    egui::FontId::monospace(30.0),
                    egui::Color32::WHITE,
                );
                painter.text(
                    egui::pos2(20.0, 56.0),
                    egui::Align2::LEFT_TOP,
                    crate::timeline::format_wall_us(wall_us).unwrap_or_default(),
                    egui::FontId::monospace(30.0),
                    egui::Color32::LIGHT_GREEN,
                );

                for (index, white) in encode_cells(ms).into_iter().enumerate() {
                    let cell = egui::Rect::from_min_size(
                        egui::pos2(STRIP_X + index as f32 * CELL_WIDTH, STRIP_Y),
                        egui::vec2(CELL_WIDTH, CELL_HEIGHT),
                    );
                    painter.rect_filled(
                        cell,
                        0.0,
                        if white {
                            egui::Color32::WHITE
                        } else {
                            egui::Color32::BLACK
                        },
                    );
                }
            });

        context.request_repaint();
    }
}

/// One decoded sample, before it is turned into a [`CalibrationSample`].
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub video_s: f64,
    pub predicted_ms: u64,
    pub observed_ms: u64,
}

/// Turn readings into a calibration verdict.
#[must_use]
pub fn evaluate(readings: &[Reading], tolerance_ms: f64) -> Calibration {
    let samples: Vec<CalibrationSample> = readings
        .iter()
        .map(|reading| {
            let error_ms = wrapped_difference_ms(reading.observed_ms, reading.predicted_ms) as f64;
            CalibrationSample {
                video_s: reading.video_s,
                predicted_mach_ns: reading.predicted_ms * 1_000_000,
                observed_mach_ns: reading.observed_ms * 1_000_000,
                error_ms,
            }
        })
        .collect();

    let max_abs_error_ms = samples
        .iter()
        .map(|sample| sample.error_ms.abs())
        .fold(0.0, f64::max);

    Calibration {
        method: "on-screen binary clock strip decoded from extracted video frames".to_string(),
        passed: !samples.is_empty() && max_abs_error_ms <= tolerance_ms,
        max_abs_error_ms,
        tolerance_ms,
        samples,
    }
}

/// Decode the strip from one frame of one display file.
fn read_strip(
    session: &Session,
    file: &str,
    file_s: f64,
    filter: &str,
) -> eyre::Result<Option<u64>> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &format!("{file_s:.3}"), "-i"])
        .arg(session.path(file))
        .args(["-frames:v", "1", "-vf", filter])
        .args(["-pix_fmt", "gray", "-f", "rawvideo", "-"])
        .output()
        .wrap_err("could not run ffmpeg")?;

    if !output.status.success() {
        eyre::bail!(
            "ffmpeg failed at {}: {}",
            timecode::format(file_s),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(decode_cells(&output.stdout))
}

fn spawn_clock() -> eyre::Result<(std::process::Child, crossbeam_channel::Receiver<[f64; 4]>)> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("selftest-clock")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .wrap_err("could not start the clock window")?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre::Report::msg("the clock window has no stdout"))?;

    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let numbers: Vec<f64> = line
                .strip_prefix("RECT ")
                .map(|rest| {
                    rest.split_whitespace()
                        .filter_map(|n| n.parse().ok())
                        .collect()
                })
                .unwrap_or_default();

            if let [x, y, w, h] = numbers[..] {
                let _ = tx.send([x, y, w, h]);
            }
        }
    });

    Ok((child, rx))
}

pub fn run(args: &SelftestArgs) -> eyre::Result<()> {
    if !frames::ffmpeg_available() {
        eyre::bail!("the self-test decodes frames with ffmpeg, which was not found on PATH");
    }

    sck::ensure_permission()?;

    let main_id = CGMainDisplayID();
    let displays = sck::active_displays()?;
    let display = displays
        .iter()
        .find(|display| display.id == main_id)
        .ok_or_else(|| eyre::Report::msg("the main display is not available for capture"))?
        .clone();

    eprintln!("starting the clock window on display {main_id}...");
    let (mut child, rect_rx) = spawn_clock()?;

    let first = rect_rx
        .recv_timeout(Duration::from_secs(20))
        .map_err(|_| eyre::Report::msg("the clock window never reported its position"))?;

    // let the window settle (and be shown) before trusting its position
    std::thread::sleep(Duration::from_millis(1500));
    let mut rect = first;
    while let Ok(latest) = rect_rx.try_recv() {
        rect = latest;
    }

    let options = RecordOptions {
        out: args.out.clone(),
        displays: vec![main_id],
        oslog: OsLogOptions {
            enabled: false,
            ..OsLogOptions::default()
        },
        duration: Some(Duration::from_secs_f64(args.seconds)),
        no_komorebi: true,
        ..RecordOptions::default()
    };

    eprintln!("recording {:.0}s...", args.seconds);
    let live = Live::new();
    let recorded = recorder::record(&options, &live);
    let _ = child.kill();
    let _ = child.wait();
    let dir = recorded?;

    let session = Session::open(&dir)?;
    let mut manifest = session.manifest()?;
    let entry = manifest
        .displays
        .iter()
        .find(|entry| entry.result.display.id == main_id && entry.result.frames > 0)
        .cloned()
        .ok_or_else(|| eyre::Report::msg("no video was recorded for the main display"))?;

    let filter = strip_filter(
        (rect[0], rect[1]),
        (display.frame[0], display.frame[1]),
        entry.result.scale,
    );

    let (_, duration) = manifest.video_span();
    let mut readings = Vec::new();
    let mut undecodable = Vec::new();
    let mut video_s = 1.0;
    while video_s <= duration - 0.5 {
        let file_s = video_s - entry.offset_s;
        let predicted_ms = manifest.mach_at_video_seconds(video_s) / 1_000_000 % WRAP_MS;
        match read_strip(&session, &entry.result.file, file_s, &filter)? {
            Some(observed_ms) => readings.push(Reading {
                video_s,
                predicted_ms,
                observed_ms,
            }),
            None => undecodable.push(video_s),
        }
        video_s += 0.5;
    }

    if readings.is_empty() {
        // leave a frame behind so the problem can be looked at
        let _ = frames::extract(&session, &manifest, &[1.0], Some(main_id));
        eyre::bail!(
            "could not find the clock in the video (is the clock window covered, or was it moved? \
             window reported at {rect:?}); a frame was saved under {}",
            session.path("frames").display()
        );
    }

    let calibration = evaluate(&readings, args.tolerance_ms);
    report(&calibration, &undecodable, &mut manifest);
    manifest.calibration = Some(calibration.clone());
    session.write_manifest(&manifest)?;
    println!("session: {}", dir.display());

    if calibration.passed {
        Ok(())
    } else {
        eyre::bail!(
            "sync self-test FAILED: the video disagrees with the clocks by up to {:.0} ms (tolerance {:.0} ms)",
            calibration.max_abs_error_ms,
            calibration.tolerance_ms
        )
    }
}

fn report(calibration: &Calibration, undecodable: &[f64], manifest: &mut Manifest) {
    println!("video second | clock shown in video vs predicted");
    for sample in &calibration.samples {
        println!(
            "  {:>6} | {:+.0} ms",
            timecode::format(sample.video_s),
            sample.error_ms
        );
    }

    let mean = calibration
        .samples
        .iter()
        .map(|sample| sample.error_ms)
        .sum::<f64>()
        / calibration.samples.len().max(1) as f64;
    println!(
        "{} samples, mean bias {mean:+.1} ms (the clock is painted slightly before it is shown), \
         worst {:.0} ms, tolerance {:.0} ms: {}",
        calibration.samples.len(),
        calibration.max_abs_error_ms,
        calibration.tolerance_ms,
        if calibration.passed { "PASS" } else { "FAIL" }
    );

    if !undecodable.is_empty() {
        let note = format!(
            "self-test could not read the clock at {} of {} sampled timestamps",
            undecodable.len(),
            undecodable.len() + calibration.samples.len()
        );
        println!("note: {note}");
        manifest.warnings.push(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(ms: u64) -> Vec<u8> {
        encode_cells(ms)
            .iter()
            .map(|white| if *white { 255 } else { 0 })
            .collect()
    }

    #[test]
    fn the_strip_roundtrips_any_clock_value() {
        for ms in [
            0,
            1,
            0xA5A5A5A5A5 & (WRAP_MS - 1),
            1_210_934_273,
            WRAP_MS - 1,
        ] {
            assert_eq!(decode_cells(&luminance(ms)), Some(ms), "{ms}");
        }
    }

    #[test]
    fn decoding_tolerates_antialiasing_but_not_a_missed_crop() {
        // slightly soft edges still decode
        let mut soft = luminance(0b1011_0010);
        for (index, level) in soft.iter_mut().enumerate() {
            *level = if *level == 255 {
                230 + (index % 5) as u8
            } else {
                10 + (index % 7) as u8
            };
        }
        assert_eq!(decode_cells(&soft), Some(0b1011_0010));

        // flat grey is a crop that missed the strip
        assert_eq!(decode_cells(&[128; CELLS]), None);
        assert_eq!(decode_cells(&[0; 10]), None, "wrong length");
    }

    #[test]
    fn wrap_around_differences_are_small() {
        assert_eq!(wrapped_difference_ms(1_050, 1_000), 50);
        assert_eq!(wrapped_difference_ms(950, 1_000), -50);
        // the strip wrapped between the prediction and the observation
        assert_eq!(wrapped_difference_ms(5, WRAP_MS - 5), 10);
        assert_eq!(wrapped_difference_ms(WRAP_MS - 5, 5), -10);
    }

    #[test]
    fn the_crop_filter_targets_the_strip_in_display_pixels() {
        // window at (80, 80) on a display at the origin, 1x
        assert_eq!(
            strip_filter((80.0, 80.0), (0.0, 0.0), 1.0),
            "crop=960:30:100:205,scale=40:1:flags=area"
        );
        // the same window on a display to the left, at 2x
        assert_eq!(
            strip_filter((-1720.0, 80.0), (-1800.0, 0.0), 2.0),
            "crop=1920:60:200:410,scale=40:1:flags=area"
        );
    }

    #[test]
    fn evaluation_passes_within_tolerance_and_reports_the_worst_error() {
        let readings = vec![
            Reading {
                video_s: 1.0,
                predicted_ms: 10_000,
                observed_ms: 9_970,
            },
            Reading {
                video_s: 1.5,
                predicted_ms: 10_500,
                observed_ms: 10_460,
            },
        ];
        let calibration = evaluate(&readings, 100.0);
        assert!(calibration.passed);
        assert!((calibration.max_abs_error_ms - 40.0).abs() < 1e-9);
        assert_eq!(calibration.samples[0].error_ms, -30.0);

        assert!(!evaluate(&readings, 35.0).passed);
        assert!(!evaluate(&[], 100.0).passed, "no readings is not a pass");
    }
}

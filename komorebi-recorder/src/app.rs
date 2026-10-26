//! The recorder window: Record / Stop, live counters and warnings.
//!
//! The window itself is excluded from the screen capture (the capture filter excludes this whole
//! application), so the controls never appear in the video.
//!
//! It is also deliberately borderless and untitled, with its own drag handle and close button.
//! komorebi does not manage windows that have no title, so opening the recorder does not
//! perturb the very layout being observed (a titled window would be tiled into the workspace).

use crate::capture::DisplayDescriptor;
use crate::capture::sck;
use crate::cli::RecordArgs;
use crate::recorder;
use crate::recorder::Live;
use crate::recorder::Phase;
use crate::timecode;
use color_eyre::eyre;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

/// Set by SIGINT/SIGTERM: the window finishes any recording in progress, then closes.
static QUIT: AtomicBool = AtomicBool::new(false);

/// How often the list of attached displays is refreshed while idle.
const DISPLAY_REFRESH: Duration = Duration::from_secs(1);

pub fn run(args: RecordArgs) -> eyre::Result<()> {
    // killing the window (or logging out) must still finalize a recording in progress
    let _ = ctrlc::set_handler(|| QUIT.store(true, Ordering::SeqCst));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // no title: komorebi leaves untitled windows alone
            .with_title("")
            .with_decorations(false)
            .with_inner_size([420.0, 380.0])
            .with_min_inner_size([360.0, 260.0])
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "komorebi-recorder",
        options,
        Box::new(|_creation_context| Ok(Box::new(RecorderApp::new(args)))),
    )
    .map_err(|error| eyre::Report::msg(format!("the recorder window failed: {error}")))
}

type Worker = JoinHandle<eyre::Result<PathBuf>>;

struct RecorderApp {
    args: RecordArgs,
    live: Arc<Live>,
    worker: Option<Worker>,
    permission: bool,
    displays: Vec<(DisplayDescriptor, bool)>,
    displays_refreshed: Instant,
    last: Option<Result<PathBuf, String>>,
    closing: bool,
    predicate: String,
    show_streams: bool,
}

impl RecorderApp {
    fn new(args: RecordArgs) -> Self {
        let mut app = Self {
            predicate: args.predicate.clone().unwrap_or_default(),
            args,
            live: Live::new(),
            worker: None,
            permission: sck::has_permission(),
            displays: Vec::new(),
            displays_refreshed: Instant::now() - DISPLAY_REFRESH,
            last: None,
            closing: false,
            show_streams: false,
        };

        app.refresh_displays();
        app
    }

    /// Re-read the attached displays (cheap, and needs no permission), keeping the ticks of the
    /// ones that are still there. Displays that were docked or undocked since the last look
    /// appear and disappear here, and a new one starts ticked.
    fn refresh_displays(&mut self) {
        self.displays_refreshed = Instant::now();
        let Ok(present) = sck::active_displays() else {
            return;
        };

        let previous: Vec<(u32, bool)> = self
            .displays
            .iter()
            .map(|(display, selected)| (display.id, *selected))
            .collect();

        self.displays = present
            .into_iter()
            .map(|display| {
                let selected = previous
                    .iter()
                    .find(|(id, _)| *id == display.id)
                    .is_none_or(|(_, selected)| *selected);
                (display, selected)
            })
            .collect();
    }

    fn recording(&self) -> bool {
        self.worker.is_some()
    }

    fn start(&mut self) {
        let mut args = self.args.clone();
        // with every display ticked, record *every* display including ones connected while
        // recording; otherwise only the ticked ones
        args.displays = if self.displays.iter().all(|(_, selected)| *selected) {
            Vec::new()
        } else {
            self.displays
                .iter()
                .filter(|(_, selected)| *selected)
                .map(|(display, _)| display.id)
                .collect()
        };
        let predicate = self.predicate.trim();
        args.predicate = (!predicate.is_empty()).then(|| predicate.to_string());

        let options = args.to_options();
        self.live.reset();
        self.last = None;

        let live = self.live.clone();
        match std::thread::Builder::new()
            .name("recorder".to_string())
            .spawn(move || recorder::record(&options, &live))
        {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => self.last = Some(Err(format!("could not start recording: {error}"))),
        }
    }

    fn poll(&mut self, context: &egui::Context) {
        if !self.recording() && self.displays_refreshed.elapsed() >= DISPLAY_REFRESH {
            self.refresh_displays();
        }

        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            self.last = Some(match worker.join() {
                Ok(Ok(dir)) => Ok(dir),
                Ok(Err(error)) => Err(format!("{error:#}")),
                Err(_) => Err("the recording thread panicked".to_string()),
            });
            self.live.reset();
        }

        let quit_signalled = QUIT.load(Ordering::SeqCst);
        if (quit_signalled || context.input(|input| input.viewport().close_requested()))
            && self.recording()
        {
            // finish and index the session before exiting rather than leaving it half-written
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.live.request_stop();
            self.closing = true;
        }

        if (self.closing || quit_signalled) && !self.recording() {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

impl eframe::App for RecorderApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(context);
        context.request_repaint_after(Duration::from_millis(200));

        let snapshot = self.live.snapshot();
        let recording = self.recording();
        let busy = matches!(snapshot.phase, Phase::Starting | Phase::Stopping);

        egui::CentralPanel::default().show(context, |ui| {
            // drag handle and close button, since there is no title bar
            let header = egui::Rect::from_min_size(
                ui.min_rect().min,
                egui::vec2(ui.available_width(), 24.0),
            );
            let drag = ui.interact(header, ui.id().with("drag"), egui::Sense::drag());
            if drag.drag_started() {
                context.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("komorebi-recorder").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("×").on_hover_text("Close").clicked() {
                        context.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            ui.separator();

            ui.horizontal(|ui| {
                let can_record = !recording && self.permission && !self.displays.is_empty();
                if ui
                    .add_enabled(
                        can_record,
                        egui::Button::new("Record").min_size([110.0, 32.0].into()),
                    )
                    .clicked()
                {
                    self.start();
                }

                if ui
                    .add_enabled(
                        recording && !busy && !self.live.stop_requested(),
                        egui::Button::new("Stop").min_size([110.0, 32.0].into()),
                    )
                    .clicked()
                {
                    self.live.request_stop();
                }

                let state = match snapshot.phase {
                    Phase::Starting => "starting...".to_string(),
                    Phase::Recording => format!("REC {}", timecode::format(snapshot.elapsed_s)),
                    Phase::Stopping => "finalizing...".to_string(),
                    _ => "ready".to_string(),
                };
                ui.label(egui::RichText::new(state).size(18.0).monospace());
            });

            ui.separator();

            if !self.permission {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Screen Recording permission is needed to record the screen.",
                );
                if ui.button("Grant permission").clicked() {
                    match sck::ensure_permission() {
                        Ok(()) => self.permission = true,
                        Err(error) => self.last = Some(Err(format!("{error:#}"))),
                    }
                }
            } else {
                ui.add_enabled_ui(!recording, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Displays:");
                        for (display, selected) in &mut self.displays {
                            ui.checkbox(
                                selected,
                                format!(
                                    "{} ({}×{})",
                                    display.id, display.width_points, display.height_points
                                ),
                            );
                        }
                    });
                });
            }

            if recording {
                ui.add_space(6.0);
                egui::Grid::new("counters").num_columns(2).show(ui, |ui| {
                    ui.label("displays");
                    ui.label(if snapshot.displays_retrying.is_empty() {
                        format!("{} recording", snapshot.displays_recording)
                    } else {
                        format!(
                            "{} recording, waiting to retry {:?}",
                            snapshot.displays_recording, snapshot.displays_retrying
                        )
                    });
                    ui.end_row();
                    ui.label("video frames");
                    ui.label(format!(
                        "{} ({} dropped)",
                        snapshot.frames, snapshot.dropped
                    ));
                    ui.end_row();
                    ui.label("komorebi log");
                    ui.label(human_bytes(snapshot.log_bytes));
                    ui.end_row();
                    ui.label("komorebi events");
                    ui.label(snapshot.events.to_string());
                    ui.end_row();
                    ui.label("OS trace");
                    ui.label(format!(
                        "{} events, {} across {} streams",
                        snapshot.oslog_events,
                        human_bytes(snapshot.oslog_bytes),
                        snapshot.streams.len()
                    ));
                    ui.end_row();
                });

                ui.collapsing("OS trace streams", |ui| {
                    self.show_streams = true;
                    for stream in &snapshot.streams {
                        ui.label(format!(
                            "{}: {} events, {}",
                            stream.name,
                            stream.events,
                            human_bytes(stream.bytes)
                        ));
                    }
                });
            } else {
                ui.collapsing("Advanced", |ui| {
                    ui.label("`system` OS trace predicate (blank for the default):");
                    ui.text_edit_singleline(&mut self.predicate);
                });
            }

            if !snapshot.incidents.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Recent events").small().weak());
                for incident in snapshot.incidents.iter().rev().take(4) {
                    ui.label(egui::RichText::new(incident).small().weak());
                }
            }

            if !snapshot.warnings.is_empty() {
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(110.0)
                    .id_salt("warnings")
                    .show(ui, |ui| {
                        for warning in &snapshot.warnings {
                            ui.colored_label(egui::Color32::YELLOW, format!("! {warning}"));
                        }
                    });
            }

            match &self.last {
                Some(Ok(dir)) => {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.label("Saved session:");
                    ui.monospace(dir.display().to_string());
                    ui.horizontal(|ui| {
                        if ui.button("Copy path").clicked() {
                            context.copy_text(dir.display().to_string());
                        }

                        if ui.button("Open in Finder").clicked() {
                            let _ = std::process::Command::new("open").arg(dir).spawn();
                        }
                    });
                    ui.label(format!(
                        "Query it: komorebi-recorder at {} <timecode>",
                        dir.display()
                    ));
                }
                Some(Err(error)) => {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                None => {}
            }
        });
    }
}

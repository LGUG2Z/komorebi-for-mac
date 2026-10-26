//! `record`, `gui` and `displays`: the arguments of a recording and the headless recorder.

use crate::capture::CaptureConfig;
use crate::capture::Codec;
use crate::capture::sck;
use crate::recorder;
use crate::recorder::AppsMode;
use crate::recorder::Live;
use crate::recorder::OsLogOptions;
use crate::recorder::RecordOptions;
use crate::session::Session;
use crate::sources::oslog::OsLogLevel;
use crate::timecode;
use clap::Args;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Args, Clone, Debug)]
pub struct RecordArgs {
    /// Session directory to create [default: <komorebi data dir>/recordings/<UTC timestamp>]
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Stop automatically after this many seconds
    #[arg(long, value_name = "SECONDS")]
    pub duration: Option<f64>,
    /// Display id to record (repeatable; see `displays`) [default: every display]
    #[arg(long = "display", value_name = "ID")]
    pub displays: Vec<u32>,
    /// Maximum video frame rate (frames are only produced when the screen changes)
    #[arg(long, default_value_t = 30)]
    pub fps: u32,
    /// Record at native pixel resolution instead of logical points
    #[arg(long)]
    pub retina: bool,
    /// Video codec
    #[arg(long, value_enum, default_value_t = Codec::Hevc)]
    pub codec: Codec,
    /// Do not capture komorebi's log or events (video and OS trace only)
    #[arg(long)]
    pub no_komorebi: bool,
    /// Do not capture the macOS unified log at all
    #[arg(long)]
    pub no_oslog: bool,
    /// Which applications get their own OS trace file
    #[arg(long, value_enum, default_value_t = AppsMode::Managed)]
    pub oslog_apps: AppsMode,
    /// Also capture one unfiltered system-wide stream (very large)
    #[arg(long)]
    pub oslog_all: bool,
    /// Unified log level to capture
    #[arg(long, value_enum, default_value_t = OsLogLevel::Debug)]
    pub oslog_level: OsLogLevel,
    /// Replace the predicate of the `system` OS trace stream
    #[arg(long, value_name = "PREDICATE")]
    pub predicate: Option<String>,
    /// Add a system daemon to the `system` stream, e.g. WindowServer (repeatable; high volume)
    #[arg(long = "system-process", value_name = "NAME")]
    pub system_processes: Vec<String>,
    /// Also trace an application by name (repeatable)
    #[arg(long = "trace-process", value_name = "NAME")]
    pub trace_processes: Vec<String>,
    /// Also trace a process by pid (repeatable)
    #[arg(long = "trace-pid", value_name = "PID")]
    pub trace_pids: Vec<i32>,
    /// Never trace this application (repeatable), e.g. a chatty one
    #[arg(long = "oslog-exclude", value_name = "NAME")]
    pub oslog_exclude: Vec<String>,
}

impl Default for RecordArgs {
    fn default() -> Self {
        Self {
            out: None,
            duration: None,
            displays: Vec::new(),
            fps: 30,
            retina: false,
            codec: Codec::Hevc,
            no_komorebi: false,
            no_oslog: false,
            oslog_apps: AppsMode::Managed,
            oslog_all: false,
            oslog_level: OsLogLevel::Debug,
            predicate: None,
            system_processes: Vec::new(),
            trace_processes: Vec::new(),
            trace_pids: Vec::new(),
            oslog_exclude: Vec::new(),
        }
    }
}

impl RecordArgs {
    #[must_use]
    pub fn to_options(&self) -> RecordOptions {
        RecordOptions {
            out: self.out.clone(),
            displays: self.displays.clone(),
            capture: CaptureConfig {
                fps: self.fps,
                retina: self.retina,
                codec: self.codec,
            },
            oslog: OsLogOptions {
                enabled: !self.no_oslog,
                apps: self.oslog_apps,
                all: self.oslog_all,
                level: self.oslog_level,
                predicate: self.predicate.clone(),
                system_processes: self.system_processes.clone(),
                trace_processes: self.trace_processes.clone(),
                trace_pids: self.trace_pids.clone(),
                exclude: self.oslog_exclude.clone(),
            },
            duration: self.duration.map(Duration::from_secs_f64),
            no_komorebi: self.no_komorebi,
        }
    }
}

pub(super) fn displays() -> eyre::Result<()> {
    for display in sck::active_displays()? {
        println!(
            "{}  {}x{} points at ({}, {})",
            display.id,
            display.width_points,
            display.height_points,
            display.frame[0],
            display.frame[1]
        );
    }

    Ok(())
}

pub(super) fn record(args: &RecordArgs) -> eyre::Result<()> {
    let live = Live::new();
    let handler_live = live.clone();
    ctrlc::set_handler(move || handler_live.request_stop())
        .wrap_err("could not install the Ctrl-C handler")?;

    let options = args.to_options();
    let worker_live = live.clone();
    let worker = std::thread::Builder::new()
        .name("recorder".to_string())
        .spawn(move || recorder::record(&options, &worker_live))?;

    let mut announced = false;
    while !worker.is_finished() {
        std::thread::sleep(Duration::from_millis(500));
        let snapshot = live.snapshot();

        if snapshot.phase == recorder::Phase::Recording {
            if !announced {
                announced = true;
                if let Some(dir) = &snapshot.session_dir {
                    eprintln!("recording to {} (Ctrl-C to stop)", dir.display());
                }
                for warning in &snapshot.warnings {
                    eprintln!("warning: {warning}");
                }
            }

            eprint!(
                "\r  {}  frames {}  komorebi log {} B  events {}  OS trace {} events   ",
                timecode::format(snapshot.elapsed_s),
                snapshot.frames,
                snapshot.log_bytes,
                snapshot.events,
                snapshot.oslog_events,
            );
        }
    }

    eprintln!();
    let dir = worker
        .join()
        .map_err(|_| eyre::Report::msg("the recording thread panicked"))??;

    let session = Session::open(&dir)?;
    let manifest = session.manifest()?;
    let (_, duration) = manifest.video_span();
    println!("session: {}", dir.display());
    println!("video:   {:.1}s", duration);
    println!(
        "recorded: {} komorebi log bytes, {} events, {} OS trace events across {} streams",
        manifest.log_segments.iter().map(|s| s.len()).sum::<u64>(),
        manifest.events.events,
        manifest
            .oslog
            .iter()
            .map(|stream| stream.events)
            .sum::<u64>(),
        manifest.oslog.len(),
    );
    for warning in &manifest.warnings {
        println!("warning: {warning}");
    }
    println!(
        "\nquery it:  komorebi-recorder at {} <timecode>\n           komorebi-recorder summary {}",
        dir.display(),
        dir.display()
    );

    Ok(())
}

//! Command line interface: argument definitions and the non-GUI commands.

use crate::capture::CaptureConfig;
use crate::capture::Codec;
use crate::capture::sck;
use crate::frames;
use crate::recorder;
use crate::recorder::AppsMode;
use crate::recorder::Live;
use crate::recorder::OsLogOptions;
use crate::recorder::RecordOptions;
use crate::report;
use crate::session::Session;
use crate::sources::oslog::OsLogLevel;
use crate::timecode;
use crate::timeline;
use crate::timeline::Filter;
use crate::timeline::Locator;
use chrono::DateTime;
use chrono::Utc;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Record the screen, komorebi's logs and events, and the macOS OS trace in lockstep",
    long_about = "Record the screen together with komorebi's log, komorebi's subscriber events and \
                  the macOS unified log (per process), all on one clock, into a session directory. \
                  Then ask what was happening at any second of the video, or where in the video any \
                  log line or event is.\n\n\
                  Run with no subcommand to open the recorder window."
)]
pub struct Opts {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Open the recorder window with Record / Stop buttons (the default)
    Gui(RecordArgs),
    /// Record without a window; Ctrl-C (or --duration) stops and finalizes the session
    Record(RecordArgs),
    /// List the displays that can be recorded
    Displays,
    /// Everything recorded around a video timecode: komorebi log, events and OS trace
    At(AtArgs),
    /// Everything recorded between two video timecodes
    Range(RangeArgs),
    /// A per-bucket digest of the whole recording
    Summary(SummaryArgs),
    /// Reverse lookup: a UTC time, log line or event number to a video timecode
    Locate(LocateArgs),
    /// Extract PNG frames at video timecodes (needs ffmpeg)
    Frames(FramesArgs),
    /// Rebuild a session's timeline from its raw files, recovering it first if the recorder died
    Reindex(ReindexArgs),
}

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

#[derive(Args, Debug)]
pub struct FilterArgs {
    /// Only this application's OS trace (exe name, case-insensitive, or a pid)
    #[arg(long, value_name = "EXE|PID")]
    pub app: Option<String>,
    /// Only these sources: komorebi-log, events, oslog, oslog:komorebi, oslog:system,
    /// oslog:app, oslog:app:<exe> (repeatable)
    #[arg(long = "source", value_name = "SOURCE")]
    pub sources: Vec<String>,
    /// Machine-readable JSON output
    #[arg(long)]
    pub json: bool,
    /// Most records to print per source in text mode (0 for no limit)
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

impl FilterArgs {
    fn filter(&self) -> Filter {
        Filter {
            app: self.app.clone(),
            sources: self.sources.clone(),
        }
    }
}

#[derive(Args, Debug)]
pub struct AtArgs {
    /// Session directory
    pub session: PathBuf,
    /// Video timecode: 12.5, 00:12.500 or 1:02:03
    pub time: String,
    /// How far either side of the timecode to look
    #[arg(long, default_value = "1s")]
    pub window: String,
    #[command(flatten)]
    pub filter: FilterArgs,
}

#[derive(Args, Debug)]
pub struct RangeArgs {
    /// Session directory
    pub session: PathBuf,
    /// Start timecode
    pub from: String,
    /// End timecode
    pub to: String,
    #[command(flatten)]
    pub filter: FilterArgs,
}

#[derive(Args, Debug)]
pub struct SummaryArgs {
    /// Session directory
    pub session: PathBuf,
    /// Bucket size
    #[arg(long, default_value = "1s")]
    pub bucket: String,
    /// List every source's count instead of log/events/os totals
    #[arg(long)]
    pub by_source: bool,
    /// Only this application's OS trace
    #[arg(long, value_name = "EXE|PID")]
    pub app: Option<String>,
    /// Only these sources (repeatable)
    #[arg(long = "source", value_name = "SOURCE")]
    pub sources: Vec<String>,
    /// Machine-readable JSON output
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("what").required(true).multiple(false)))]
pub struct LocateArgs {
    /// Session directory
    pub session: PathBuf,
    /// A UTC time (RFC 3339), e.g. 2026-10-26T19:43:35.062613Z
    #[arg(long, group = "what")]
    pub utc: Option<String>,
    /// A 1-based line number in the session's komorebi.log
    #[arg(long, group = "what")]
    pub log_line: Option<u64>,
    /// A subscriber event sequence number from events.jsonl
    #[arg(long, group = "what")]
    pub event_seq: Option<u64>,
    /// Machine-readable JSON output
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct FramesArgs {
    /// Session directory
    pub session: PathBuf,
    /// Video timecodes to extract
    #[arg(required = true)]
    pub times: Vec<String>,
    /// Only this display
    #[arg(long, value_name = "ID")]
    pub display: Option<u32>,
    /// Machine-readable JSON output
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct ReindexArgs {
    /// Session directory
    pub session: PathBuf,
}

/// Print `value` as pretty JSON.
fn print_json(value: &impl Serialize) -> eyre::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[derive(Serialize)]
struct QueryOutput<'a> {
    session: String,
    from_s: f64,
    to_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    center_s: Option<f64>,
    count: usize,
    records: &'a [timeline::Record],
}

fn limit(value: usize) -> Option<usize> {
    (value != 0).then_some(value)
}

pub fn run(command: Command) -> eyre::Result<()> {
    match command {
        Command::Gui(args) => crate::app::run(args),
        Command::Record(args) => record(&args),
        Command::Displays => displays(),
        Command::At(args) => at(&args),
        Command::Range(args) => range(&args),
        Command::Summary(args) => summary(&args),
        Command::Locate(args) => locate(&args),
        Command::Frames(args) => frames_command(&args),
        Command::Reindex(args) => reindex(&args),
    }
}

fn displays() -> eyre::Result<()> {
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

fn record(args: &RecordArgs) -> eyre::Result<()> {
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

fn load(session: &std::path::Path) -> eyre::Result<(Session, crate::session::Manifest)> {
    let session = Session::open(session)?;
    let manifest = session.manifest()?;

    if manifest.status == crate::session::STATUS_RECORDING {
        eyre::bail!(
            "{} is still being recorded, or the recorder was killed or crashed before finishing it; \
             if it is no longer running, recover it with `komorebi-recorder reindex {}`",
            session.dir.display(),
            session.dir.display()
        );
    }

    Ok((session, manifest))
}

/// When `--app` matched nothing, say which applications this session does have.
fn hint_unknown_app(manifest: &crate::session::Manifest, filter: &FilterArgs, empty: bool) {
    let Some(app) = &filter.app else {
        return;
    };

    let known = timeline::app_names(manifest);
    let is_known =
        app.parse::<i64>().is_ok() || known.iter().any(|name| name.eq_ignore_ascii_case(app));
    if empty && !is_known {
        eprintln!(
            "note: no application '{app}' was traced in this session; traced: {}",
            if known.is_empty() {
                "(none)".to_string()
            } else {
                known.join(", ")
            }
        );
    }
}

fn at(args: &AtArgs) -> eyre::Result<()> {
    let (session, manifest) = load(&args.session)?;
    let center = timecode::parse(&args.time)?;
    let window = timecode::parse_span(&args.window)?;
    let (from, to) = (center - window, center + window);

    let records = timeline::query(&session, &manifest, from, to, &args.filter.filter())?;

    if args.filter.json {
        return print_json(&QueryOutput {
            session: session.dir.display().to_string(),
            from_s: from,
            to_s: to,
            center_s: Some(center),
            count: records.len(),
            records: &records,
        });
    }

    hint_unknown_app(&manifest, &args.filter, records.is_empty());

    let heading = format!(
        "{} (window +-{window}s: {} to {})",
        timecode::format(center),
        timecode::format(from),
        timecode::format(to)
    );
    print!(
        "{}",
        report::records_text(&heading, &records, Some(center), limit(args.filter.limit))
    );
    Ok(())
}

fn range(args: &RangeArgs) -> eyre::Result<()> {
    let (session, manifest) = load(&args.session)?;
    let from = timecode::parse(&args.from)?;
    let to = timecode::parse(&args.to)?;
    if to < from {
        eyre::bail!(
            "the range end {} is before its start {}",
            args.to,
            args.from
        );
    }

    let records = timeline::query(&session, &manifest, from, to, &args.filter.filter())?;

    if args.filter.json {
        return print_json(&QueryOutput {
            session: session.dir.display().to_string(),
            from_s: from,
            to_s: to,
            center_s: None,
            count: records.len(),
            records: &records,
        });
    }

    hint_unknown_app(&manifest, &args.filter, records.is_empty());

    let heading = format!("{} to {}", timecode::format(from), timecode::format(to));
    print!(
        "{}",
        report::records_text(&heading, &records, None, limit(args.filter.limit))
    );
    Ok(())
}

fn summary(args: &SummaryArgs) -> eyre::Result<()> {
    let (session, manifest) = load(&args.session)?;
    let bucket = timecode::parse_span(&args.bucket)?;
    let filter = Filter {
        app: args.app.clone(),
        sources: args.sources.clone(),
    };
    let buckets = timeline::summary(&session, &manifest, bucket, &filter)?;

    if args.json {
        return print_json(&buckets);
    }

    print!("{}", report::summary_text(&buckets, args.by_source));
    Ok(())
}

fn locate(args: &LocateArgs) -> eyre::Result<()> {
    let (session, manifest) = load(&args.session)?;
    let locator = if let Some(utc) = &args.utc {
        let time: DateTime<Utc> = DateTime::parse_from_rfc3339(utc)
            .wrap_err_with(|| {
                format!("'{utc}' is not an RFC 3339 time like 2026-10-26T19:43:35.062613Z")
            })?
            .with_timezone(&Utc);
        Locator::Utc(time)
    } else if let Some(line) = args.log_line {
        Locator::LogLine(line)
    } else if let Some(seq) = args.event_seq {
        Locator::EventSeq(seq)
    } else {
        eyre::bail!("one of --utc, --log-line or --event-seq is required");
    };

    let located = timeline::locate(&session, &manifest, &locator)?;
    if args.json {
        return print_json(&located);
    }

    print!("{}", report::locate_text(&located));
    Ok(())
}

fn frames_command(args: &FramesArgs) -> eyre::Result<()> {
    let (session, manifest) = load(&args.session)?;
    let times = args
        .times
        .iter()
        .map(|time| timecode::parse(time))
        .collect::<eyre::Result<Vec<_>>>()?;

    let extraction = frames::extract(&session, &manifest, &times, args.display)?;
    if args.json {
        return print_json(&extraction);
    }

    for frame in &extraction.frames {
        println!(
            "{}  display {}  {}",
            timecode::format(frame.video_s),
            frame.display,
            frame.path.display()
        );
    }

    for skipped in &extraction.skipped {
        eprintln!(
            "note: {}  display {}: {}",
            timecode::format(skipped.video_s),
            skipped.display,
            skipped.reason
        );
    }

    Ok(())
}

fn reindex(args: &ReindexArgs) -> eyre::Result<()> {
    let session = Session::open(&args.session)?;

    // a session whose recorder died is closed out first
    for note in crate::recovery::recover(&session)? {
        println!("recovered: {note}");
    }

    let stats = timeline::build(&session)?;
    session.write_readme(&session.manifest()?)?;
    println!("indexed {} records", stats.entries);
    for (source, count) in &stats.by_source {
        println!("  {source}: {count}");
    }

    Ok(())
}

//! Command line interface: the command table and the commands that need no module of their own.
//!
//! Each family of commands lives next to its arguments: [`record`] (recording), [`query`]
//! (reading a session back).

mod annotate;
mod query;
mod record;

use crate::analysis::envelope::unusable;
use crate::analysis::envelope::with_hint;
use crate::session::Session;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use color_eyre::eyre;
use serde::Serialize;
use std::path::PathBuf;

pub use annotate::AnnotateArgs;
pub use annotate::AnnotationsArgs;
pub use query::AtArgs;
pub use query::FilterArgs;
pub use query::FramesArgs;
pub use query::LocateArgs;
pub use query::RangeArgs;
pub use query::SummaryArgs;
pub use record::RecordArgs;

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
    /// Mark where a bug starts and ends, add notes, or record an agent's finding
    Annotate(AnnotateArgs),
    /// List a session's bug regions, notes and findings
    Annotations(AnnotationsArgs),
    /// Rebuild a session's timeline from its raw files, recovering it first if the recorder died
    Reindex(ReindexArgs),
    /// Check the video-to-clock alignment against a rendered on-screen clock
    Selftest(SelftestArgs),
    /// The clock window used by `selftest` (internal)
    #[command(hide = true)]
    SelftestClock,
}

#[derive(Args, Debug)]
pub struct ReindexArgs {
    /// Session directory
    pub session: PathBuf,
}

#[derive(Args, Debug)]
pub struct SelftestArgs {
    /// Seconds to record
    #[arg(long, default_value_t = 5.0)]
    pub seconds: f64,
    /// Largest allowed disagreement between the video and the clocks, in milliseconds
    #[arg(long, default_value_t = 100.0)]
    pub tolerance_ms: f64,
    /// Session directory for the self-test recording
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// Print `value` as pretty JSON.
pub(crate) fn print_json(value: &impl Serialize) -> eyre::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub(crate) fn limit(value: usize) -> Option<usize> {
    (value != 0).then_some(value)
}

impl Command {
    /// Whether this command was asked for JSON, which also decides how a failure is reported.
    #[must_use]
    pub fn wants_json(&self) -> bool {
        match self {
            Self::At(args) => args.filter.json,
            Self::Range(args) => args.filter.json,
            Self::Summary(args) => args.json,
            Self::Locate(args) => args.json,
            Self::Frames(args) => args.json,
            Self::Annotate(args) => args.json(),
            Self::Annotations(args) => args.json,
            _ => false,
        }
    }
}

/// Run `command` and turn its outcome into the process exit status: 0, or the code that says what
/// kind of failure it was (see [`crate::analysis::envelope::ExitKind`]). With `--json` a failure
/// is also printed as an envelope so that a caller parsing stdout still gets an answer.
pub fn execute(command: Command) -> std::process::ExitCode {
    let json = command.wants_json();
    match run(command) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            let kind = crate::analysis::envelope::exit_kind(&error);
            if json {
                let envelope = crate::analysis::envelope::Envelope::failed(&error);
                if let Ok(text) = serde_json::to_string_pretty(&envelope) {
                    println!("{text}");
                }
            }
            eprintln!("Error: {error:?}");
            std::process::ExitCode::from(kind.code())
        }
    }
}

pub fn run(command: Command) -> eyre::Result<()> {
    match command {
        Command::Gui(args) => crate::app::run(args),
        Command::Record(args) => record::record(&args),
        Command::Displays => record::displays(),
        Command::At(args) => query::at(&args),
        Command::Range(args) => query::range(&args),
        Command::Summary(args) => query::summary(&args),
        Command::Locate(args) => query::locate(&args),
        Command::Frames(args) => query::frames_command(&args),
        Command::Annotate(args) => annotate::annotate(&args),
        Command::Annotations(args) => annotate::list(&args),
        Command::Reindex(args) => reindex(&args),
        Command::Selftest(args) => crate::selftest::run(&args),
        Command::SelftestClock => crate::selftest::run_clock(),
    }
}

pub(crate) fn load(session: &std::path::Path) -> eyre::Result<(Session, crate::session::Manifest)> {
    let session = Session::open(session)?;
    let manifest = session.manifest()?;

    if manifest.status == crate::session::STATUS_RECORDING {
        let dir = session.dir.display();
        return Err(match crate::lock::recorder(&session.dir) {
            crate::lock::Recorder::Running(_) => unusable(format!(
                "{dir} is still being recorded; stop the recording first"
            )),
            crate::lock::Recorder::NotRunning | crate::lock::Recorder::Unknown => with_hint(
                unusable(format!(
                    "{dir} was left unfinished: its recorder was killed or crashed"
                )),
                format!("recover it with `komorebi-recorder reindex {dir}`"),
            ),
        });
    }

    Ok((session, manifest))
}

fn reindex(args: &ReindexArgs) -> eyre::Result<()> {
    let session = Session::open(&args.session)?;
    let reindexed = crate::recovery::reindex_session(&session)?;

    for note in &reindexed.recovered {
        println!("recovered: {note}");
    }

    println!("indexed {} records", reindexed.stats.entries);
    for (source, count) in &reindexed.stats.by_source {
        println!("  {source}: {count}");
    }

    Ok(())
}

use clap::Parser;
use color_eyre::eyre;
use komorebi_recorder::cli::Command;
use komorebi_recorder::cli::Opts;
use komorebi_recorder::cli::RecordArgs;
use std::process::ExitCode;

fn main() -> eyre::Result<ExitCode> {
    color_eyre::install()?;

    let opts = Opts::parse();
    Ok(komorebi_recorder::cli::execute(
        opts.command
            .unwrap_or_else(|| Command::Gui(RecordArgs::default())),
    ))
}

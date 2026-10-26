use clap::Parser;
use color_eyre::eyre;
use komorebi_recorder::cli::Command;
use komorebi_recorder::cli::Opts;
use komorebi_recorder::cli::RecordArgs;

fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    let opts = Opts::parse();
    komorebi_recorder::cli::run(
        opts.command
            .unwrap_or_else(|| Command::Gui(RecordArgs::default())),
    )
}

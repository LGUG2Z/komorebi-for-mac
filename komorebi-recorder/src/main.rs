use clap::CommandFactory;
use clap::Parser;
use color_eyre::eyre;
use komorebi_recorder::cli::Opts;

fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    match Opts::parse().command {
        Some(command) => komorebi_recorder::cli::run(command),
        None => {
            Opts::command().print_help()?;
            Ok(())
        }
    }
}

#![warn(clippy::all)]

use clap::Parser;
use color_eyre::eyre;
use komorebi_client::Axis;
use komorebi_client::CycleDirection;
use komorebi_client::DefaultLayout;
use komorebi_client::OperationDirection;
use komorebi_client::PathExt;
use komorebi_client::Sizing;
use komorebi_client::SocketMessage;
use komorebi_client::send_message;
use lazy_static::lazy_static;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

lazy_static! {
    static ref HAS_CUSTOM_CONFIG_HOME: AtomicBool = AtomicBool::new(false);
    static ref HOME_DIR: PathBuf = {
        std::env::var("KOMOREBI_CONFIG_HOME").map_or_else(
            |_| dirs::home_dir().expect("there is no home directory"),
            |home_path| {
                let home = home_path.replace_env();
                if home.as_path().is_dir() {
                    HAS_CUSTOM_CONFIG_HOME.store(true, Ordering::SeqCst);
                    home
                } else {
                    panic!(
                        "$Env:KOMOREBI_CONFIG_HOME is set to '{home_path}', which is not a valid directory",
                    );
                }
            },
        )
    };
    static ref DATA_DIR: PathBuf = dirs::data_local_dir()
        .expect("there is no local data directory")
        .join("komorebi");
}

macro_rules! gen_enum_subcommand_args {
    // SubCommand Pattern: Enum Type
    ( $( $name:ident: $element:ty ),+ $(,)? ) => {
        $(
            pastey::paste! {
                #[derive(clap::Parser)]
                pub struct $name {
                    #[clap(value_enum)]
                    [<$element:snake>]: $element
                }
            }
        )+
    };
}

gen_enum_subcommand_args! {
    Focus: OperationDirection,
    Move: OperationDirection,
    ChangeLayout: DefaultLayout,
    Stack: OperationDirection,
    CycleStack: CycleDirection,
}

macro_rules! gen_target_subcommand_args {
    // SubCommand Pattern
    ( $( $name:ident ),+ $(,)? ) => {
        $(
            #[derive(clap::Parser)]
            pub struct $name {
                /// Target index (zero-indexed)
                target: usize,
            }
        )+
    };
}

gen_target_subcommand_args! {
    FocusWorkspace,
    MoveToWorkspace,
    SendToWorkspace
}

#[derive(Parser)]
struct Resize {
    #[clap(value_enum)]
    edge: OperationDirection,
    #[clap(value_enum)]
    sizing: Sizing,
}

#[derive(Parser)]
struct ResizeAxis {
    #[clap(value_enum)]
    axis: Axis,
    #[clap(value_enum)]
    sizing: Sizing,
}

#[derive(Parser)]
#[clap(author, about, version)]
struct Opts {
    #[clap(subcommand)]
    subcmd: SubCommand,
}

#[derive(Parser)]
enum SubCommand {
    /// Change focus to the window in the specified direction
    #[clap(arg_required_else_help = true)]
    Focus(Focus),
    /// Move the focused window in the specified direction
    #[clap(arg_required_else_help = true)]
    Move(Move),
    /// Stack the focused window in the specified direction
    #[clap(arg_required_else_help = true)]
    Stack(Stack),
    /// Unstack the focused window
    Unstack,
    /// Cycle the focused stack in the specified cycle direction
    #[clap(arg_required_else_help = true)]
    CycleStack(CycleStack),
    /// Resize the focused window in the specified direction
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "resize")]
    ResizeEdge(Resize),
    /// Resize the focused window or primary column along the specified axis
    #[clap(arg_required_else_help = true)]
    ResizeAxis(ResizeAxis),
    /// Toggle the paused state for all window tiling
    TogglePause,
    /// Toggle monocle mode for the focused container
    ToggleMonocle,
    /// Toggle floating mode for the focused window
    ToggleFloat,
    /// Toggle between the Tiling and Floating layers on the focused workspace
    ToggleWorkspaceLayer,
    /// Set the layout on the focused workspace
    #[clap(arg_required_else_help = true)]
    ChangeLayout(ChangeLayout),
    /// Focus the specified workspace on the focused monitor
    #[clap(arg_required_else_help = true)]
    FocusWorkspace(FocusWorkspace),
    /// Move the focused window to the specified workspace
    #[clap(arg_required_else_help = true)]
    MoveToWorkspace(MoveToWorkspace),
    /// Send the focused window to the specified workspace
    #[clap(arg_required_else_help = true)]
    SendToWorkspace(SendToWorkspace),
    /// Force the retiling of all managed windows
    Retile,
}

fn main() -> eyre::Result<()> {
    let opts: Opts = Opts::parse();

    match opts.subcmd {
        SubCommand::Focus(arg) => {
            send_message(&SocketMessage::FocusWindow(arg.operation_direction))?;
        }
        SubCommand::Move(arg) => {
            send_message(&SocketMessage::MoveWindow(arg.operation_direction))?;
        }
        SubCommand::TogglePause => {
            send_message(&SocketMessage::TogglePause)?;
        }
        SubCommand::ChangeLayout(arg) => {
            send_message(&SocketMessage::ChangeLayout(arg.default_layout))?;
        }
        SubCommand::Stack(arg) => {
            send_message(&SocketMessage::StackWindow(arg.operation_direction))?;
        }
        SubCommand::Unstack => {
            send_message(&SocketMessage::UnstackWindow)?;
        }
        SubCommand::CycleStack(arg) => {
            send_message(&SocketMessage::CycleStack(arg.cycle_direction))?;
        }
        SubCommand::FocusWorkspace(arg) => {
            send_message(&SocketMessage::FocusWorkspaceNumber(arg.target))?;
        }
        SubCommand::MoveToWorkspace(arg) => {
            send_message(&SocketMessage::MoveContainerToWorkspaceNumber(arg.target))?;
        }
        SubCommand::SendToWorkspace(arg) => {
            send_message(&SocketMessage::SendContainerToWorkspaceNumber(arg.target))?;
        }
        SubCommand::ToggleMonocle => {
            send_message(&SocketMessage::ToggleMonocle)?;
        }
        SubCommand::ToggleFloat => {
            send_message(&SocketMessage::ToggleFloat)?;
        }
        SubCommand::ToggleWorkspaceLayer => {
            send_message(&SocketMessage::ToggleWorkspaceLayer)?;
        }
        SubCommand::ResizeEdge(arg) => {
            send_message(&SocketMessage::ResizeWindowEdge(arg.edge, arg.sizing))?;
        }
        SubCommand::ResizeAxis(arg) => {
            send_message(&SocketMessage::ResizeWindowAxis(arg.axis, arg.sizing))?;
        }
        SubCommand::Retile => {
            send_message(&SocketMessage::Retile)?;
        }
    }

    Ok(())
}

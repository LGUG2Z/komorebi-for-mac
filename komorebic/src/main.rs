#![warn(clippy::all)]

use chrono::Utc;
use clap::Parser;
use color_eyre::eyre;
use fs_tail::TailedFile;
use komorebi_client::Axis;
use komorebi_client::CycleDirection;
use komorebi_client::DefaultLayout;
use komorebi_client::OperationDirection;
use komorebi_client::PathExt;
use komorebi_client::Sizing;
use komorebi_client::SocketMessage;
use komorebi_client::replace_env_in_path;
use komorebi_client::send_message;
use komorebi_client::send_query;
use lazy_static::lazy_static;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use sysinfo::ProcessesToUpdate;
use sysinfo::Signal;

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
    CycleFocus: CycleDirection,
    CycleMove: CycleDirection,
    CycleMoveToWorkspace: CycleDirection,
    CycleSendToWorkspace: CycleDirection,
    CycleSendToMonitor: CycleDirection,
    CycleMoveToMonitor: CycleDirection,
    CycleMonitor: CycleDirection,
    CycleWorkspace: CycleDirection,
    CycleEmptyWorkspace: CycleDirection,
    CycleMoveWorkspaceToMonitor: CycleDirection,
    Stack: OperationDirection,
    CycleStack: CycleDirection,
    CycleStackIndex: CycleDirection,
    FlipLayout: Axis,
    ChangeLayout: DefaultLayout,
    CycleLayout: CycleDirection,
    // WatchConfiguration: BooleanState,
    // MouseFollowsFocus: BooleanState,
    // Query: StateQuery,
    // WindowHidingBehaviour: HidingBehaviour,
    // CrossMonitorMoveBehaviour: MoveBehaviour,
    // UnmanagedWindowOperationBehaviour: OperationBehaviour,
    PromoteWindow: OperationDirection,
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
    MoveToMonitor,
    MoveToWorkspace,
    SendToMonitor,
    SendToWorkspace,
    FocusMonitor,
    FocusWorkspace,
    FocusWorkspaces,
    MoveWorkspaceToMonitor,
    SwapWorkspacesWithMonitor,
    FocusStackWindow,
}

macro_rules! gen_named_target_subcommand_args {
    // SubCommand Pattern
    ( $( $name:ident ),+ $(,)? ) => {
        $(
            #[derive(clap::Parser)]
            pub struct $name {
                /// Target workspace name
                workspace: String,
            }
        )+
    };
}

gen_named_target_subcommand_args! {
    MoveToNamedWorkspace,
    SendToNamedWorkspace,
    FocusNamedWorkspace,
    ClearNamedWorkspaceLayoutRules
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
struct FocusMonitorWorkspace {
    /// Target monitor index (zero-indexed)
    target_monitor: usize,
    /// Workspace index on the target monitor (zero-indexed)
    target_workspace: usize,
}

#[derive(Parser)]
pub struct SendToMonitorWorkspace {
    /// Target monitor index (zero-indexed)
    target_monitor: usize,
    /// Workspace index on the target monitor (zero-indexed)
    target_workspace: usize,
}

#[derive(Parser)]
pub struct MoveToMonitorWorkspace {
    /// Target monitor index (zero-indexed)
    target_monitor: usize,
    /// Workspace index on the target monitor (zero-indexed)
    target_workspace: usize,
}

#[derive(Parser)]
#[allow(clippy::struct_excessive_bools)]
struct Start {
    /// Path to a static configuration JSON file
    #[clap(short, long)]
    #[clap(value_parser = replace_env_in_path)]
    config: Option<PathBuf>,
    // /// Do not attempt to auto-apply a dumped state temp file from a previously running instance of komorebi
    // #[clap(long)]
    // clean_state: bool,
}

#[derive(Parser)]
#[clap(author, about, version)]
struct Opts {
    #[clap(subcommand)]
    subcmd: SubCommand,
}

#[derive(Parser)]
enum SubCommand {
    /// Start komorebi as a background process
    Start(Start),
    /// Stop the komorebi process and restore all hidden windows
    Stop,
    /// Show the path to komorebi.json
    #[clap(alias = "config")]
    Configuration,
    /// Show the path to komorebi's data directory in $HOME/Library/Application Support
    #[clap(alias = "datadir")]
    DataDirectory,
    /// Tail komorebi's process logs (cancel with Ctrl-C)
    Log,
    /// Show a JSON representation of the current window manager state
    State,
    /// Show a JSON representation of the current global state
    GlobalState,
    /// Show a JSON representation of visible windows
    VisibleWindows,
    /// Show information about connected monitors
    #[clap(alias = "monitor-info")]
    MonitorInformation,
    /// Change focus to the window in the specified direction
    #[clap(arg_required_else_help = true)]
    Focus(Focus),
    /// Move the focused window in the specified direction
    #[clap(arg_required_else_help = true)]
    Move(Move),
    /// Stack the focused window in the specified direction
    #[clap(arg_required_else_help = true)]
    Stack(Stack),
    /// Change focus to the window in the specified cycle direction
    #[clap(arg_required_else_help = true)]
    CycleFocus(CycleFocus),
    /// Move the focused window in the specified cycle direction
    #[clap(arg_required_else_help = true)]
    CycleMove(CycleMove),
    /// Unstack the focused window
    Unstack,
    /// Cycle the focused stack in the specified cycle direction
    #[clap(arg_required_else_help = true)]
    CycleStack(CycleStack),
    /// Cycle the index of the focused window in the focused stack in the specified cycle direction
    #[clap(arg_required_else_help = true)]
    CycleStackIndex(CycleStackIndex),
    /// Focus the specified window index in the focused stack
    #[clap(arg_required_else_help = true)]
    FocusStackWindow(FocusStackWindow),
    /// Stack all windows on the focused workspace
    StackAll,
    /// Unstack all windows in the focused container
    UnstackAll,
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
    /// Cycle between available layouts
    #[clap(arg_required_else_help = true)]
    CycleLayout(CycleLayout),
    /// Flip the layout on the focused workspace
    #[clap(arg_required_else_help = true)]
    FlipLayout(FlipLayout),
    /// Promote the focused window to the top of the tree
    Promote,
    /// Promote the user focus to the top of the tree
    PromoteFocus,
    /// Promote the window in the specified direction
    PromoteWindow(PromoteWindow),
    /// Move the focused window to the specified monitor
    #[clap(arg_required_else_help = true)]
    MoveToMonitor(MoveToMonitor),
    /// Move the focused window to the monitor in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleMoveToMonitor(CycleMoveToMonitor),
    /// Move the focused window to the specified workspace
    #[clap(arg_required_else_help = true)]
    MoveToWorkspace(MoveToWorkspace),
    /// Move the focused window to the specified workspace
    #[clap(arg_required_else_help = true)]
    MoveToNamedWorkspace(MoveToNamedWorkspace),
    /// Move the focused window to the workspace in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleMoveToWorkspace(CycleMoveToWorkspace),
    /// Send the focused window to the specified monitor
    #[clap(arg_required_else_help = true)]
    SendToMonitor(SendToMonitor),
    /// Send the focused window to the monitor in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleSendToMonitor(CycleSendToMonitor),
    /// Send the focused window to the specified workspace
    #[clap(arg_required_else_help = true)]
    SendToWorkspace(SendToWorkspace),
    /// Send the focused window to the specified workspace
    #[clap(arg_required_else_help = true)]
    SendToNamedWorkspace(SendToNamedWorkspace),
    /// Send the focused window to the workspace in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleSendToWorkspace(CycleSendToWorkspace),
    /// Send the focused window to the specified monitor workspace
    #[clap(arg_required_else_help = true)]
    SendToMonitorWorkspace(SendToMonitorWorkspace),
    /// Move the focused window to the specified monitor workspace
    #[clap(arg_required_else_help = true)]
    MoveToMonitorWorkspace(MoveToMonitorWorkspace),
    /// Send the focused window to the last focused monitor workspace
    SendToLastWorkspace,
    /// Move the focused window to the last focused monitor workspace
    MoveToLastWorkspace,
    /// Move the focused workspace to the specified monitor
    #[clap(arg_required_else_help = true)]
    MoveWorkspaceToMonitor(MoveWorkspaceToMonitor),
    /// Move the focused workspace monitor in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleMoveWorkspaceToMonitor(CycleMoveWorkspaceToMonitor),
    /// Swap focused monitor workspaces with specified monitor
    #[clap(arg_required_else_help = true)]
    SwapWorkspacesWithMonitor(SwapWorkspacesWithMonitor),
    /// Focus the specified monitor
    #[clap(arg_required_else_help = true)]
    FocusMonitor(FocusMonitor),
    /// Focus the monitor at the current cursor location
    FocusMonitorAtCursor,
    /// Focus the last focused workspace on the focused monitor
    FocusLastWorkspace,
    /// Focus the specified workspace on the focused monitor
    #[clap(arg_required_else_help = true)]
    FocusWorkspace(FocusWorkspace),
    /// Focus the specified workspace on all monitors
    #[clap(arg_required_else_help = true)]
    FocusWorkspaces(FocusWorkspaces),
    /// Focus the specified workspace on the target monitor
    #[clap(arg_required_else_help = true)]
    FocusMonitorWorkspace(FocusMonitorWorkspace),
    /// Focus the specified workspace
    #[clap(arg_required_else_help = true)]
    FocusNamedWorkspace(FocusNamedWorkspace),
    /// Close the focused workspace (must be empty and unnamed)
    CloseWorkspace,
    /// Focus the monitor in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleMonitor(CycleMonitor),
    /// Focus the workspace in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleWorkspace(CycleWorkspace),
    /// Focus the next empty workspace in the given cycle direction (if one exists)
    #[clap(arg_required_else_help = true)]
    CycleEmptyWorkspace(CycleEmptyWorkspace),
    /// Force the retiling of all managed windows
    Retile,
    /// Toggle application of the window-based work area offset for the focused workspace
    ToggleWindowBasedWorkAreaOffset,
    /// Toggle the behaviour for new windows (stacking or dynamic tiling)
    ToggleWindowContainerBehaviour,
    /// Enable or disable float override, which makes it so every new window opens in floating mode
    ToggleFloatOverride,
    /// Toggle the behaviour for new windows (stacking or dynamic tiling) for currently focused
    /// workspace. If there was no behaviour set for the workspace previously it takes the opposite
    /// of the global value.
    ToggleWorkspaceWindowContainerBehaviour,
    /// Enable or disable float override, which makes it so every new window opens in floating
    /// mode, for the currently focused workspace. If there was no override value set for the
    /// workspace previously it takes the opposite of the global value.
    ToggleWorkspaceFloatOverride,
    /// Toggle window tiling on the focused workspace
    ToggleTiling,
    /// Toggle a lock for the focused container, ensuring it will not be displaced by any new windows
    ToggleLock,
    /// Toggle the behaviour when moving windows across monitor boundaries
    ToggleCrossMonitorMoveBehaviour,
    /// Fetch the latest version of applications.json from komorebi-application-specific-configuration
    #[clap(alias = "fetch-asc")]
    FetchAppSpecificConfiguration,
}

fn print_query(message: &SocketMessage) {
    match send_query(message) {
        Ok(response) => println!("{response}"),
        Err(error) => panic!("{}", error),
    }
}

fn main() -> eyre::Result<()> {
    let opts: Opts = Opts::parse();

    match opts.subcmd {
        SubCommand::Start(arg) => {
            let mut command = &mut Command::new("komorebi");

            if let Some(config) = &arg.config {
                command =
                    command.args(["--config", &format!("'--config=\"{}\"'", config.display())])
            };

            command = command
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null());

            let mut system = sysinfo::System::new_all();
            system.refresh_processes(ProcessesToUpdate::All, true);

            let mut attempts = 0;
            let mut running = system
                .processes_by_exact_name("komorebi".as_ref())
                .next()
                .is_some();

            while !running && attempts <= 2 {
                command.spawn()?;

                print!("Waiting for komorebi to start...");
                std::thread::sleep(Duration::from_secs(3));

                system.refresh_processes(ProcessesToUpdate::All, true);

                if system
                    .processes_by_exact_name("komorebi".as_ref())
                    .next()
                    .is_some()
                {
                    println!("Started!");
                    running = true;
                } else {
                    println!("komorebi did not start... Trying again");
                    attempts += 1;
                }
            }

            if !running {
                println!("\nRunning komorebi directly for detailed error output\n");
                if let Some(config) = &arg.config {
                    if let Ok(output) = Command::new("komorebi")
                        .arg(format!("'--config=\"{}\"'", config.display()))
                        .output()
                    {
                        println!("{}", String::from_utf8(output.stderr)?);
                    }
                } else if let Ok(output) = Command::new("komorebi").output() {
                    println!("{}", String::from_utf8(output.stderr)?);
                }

                return Ok(());
            }

            println!("\nThank you for using komorebi!\n");
            println!("# Commercial Use License");
            println!(
                "* View licensing options https://lgug2z.com/software/komorebi - A commercial use license is required to use komorebi at work"
            );
            println!("\n# Personal Use Sponsorship");
            println!(
                "* Become a sponsor https://github.com/sponsors/LGUG2Z - $5/month makes a big difference"
            );
            println!("* Leave a tip https://ko-fi.com/lgug2z - An alternative to GitHub Sponsors");
            println!("\n# Community");
            println!(
                "* Join the Discord https://discord.gg/mGkn66PHkx - Chat, ask questions, share your desktops"
            );
            println!(
                "* Subscribe to https://youtube.com/@LGUG2Z - Development videos, feature previews and release overviews"
            );
            println!(
                "* Explore the Awesome Komorebi list https://github.com/LGUG2Z/awesome-komorebi - Projects in the komorebi ecosystem"
            );
            println!("\n# Documentation");
            println!(
                "* Read the docs https://lgug2z.github.io/komorebi - Quickly search through all komorebic commands"
            );
        }
        SubCommand::Stop => {
            let mut system = sysinfo::System::new_all();
            system.refresh_processes(ProcessesToUpdate::All, true);

            let running = system.processes_by_exact_name("komorebi".as_ref()).next();

            if let Some(process) = running {
                process.kill_with(Signal::Interrupt);
            }
        }
        SubCommand::Configuration => {
            let static_config = HOME_DIR.join("komorebi.json");

            if static_config.exists() {
                println!("{}", static_config.display());
            }
        }
        SubCommand::DataDirectory => {
            let dir = &*DATA_DIR;
            if dir.exists() {
                println!("{}", dir.display());
            }
        }
        SubCommand::Log => {
            let timestamp = Utc::now().format("%Y-%m-%d").to_string();
            let color_log = DATA_DIR.join(format!("komorebi.log.{timestamp}"));
            let file = TailedFile::new(File::open(color_log)?);
            let locked = file.lock();
            #[allow(clippy::significant_drop_in_scrutinee, clippy::lines_filter_map_ok)]
            for line in locked.lines().flatten() {
                println!("{line}");
            }
        }
        SubCommand::Focus(arg) => {
            send_message(&SocketMessage::FocusWindow(arg.operation_direction))?;
        }
        SubCommand::Move(arg) => {
            send_message(&SocketMessage::MoveWindow(arg.operation_direction))?;
        }
        SubCommand::CycleFocus(arg) => {
            send_message(&SocketMessage::CycleFocusWindow(arg.cycle_direction))?;
        }
        SubCommand::CycleMove(arg) => {
            send_message(&SocketMessage::CycleMoveWindow(arg.cycle_direction))?;
        }
        SubCommand::TogglePause => {
            send_message(&SocketMessage::TogglePause)?;
        }
        SubCommand::ChangeLayout(arg) => {
            send_message(&SocketMessage::ChangeLayout(arg.default_layout))?;
        }
        SubCommand::CycleLayout(arg) => {
            send_message(&SocketMessage::CycleLayout(arg.cycle_direction))?;
        }
        SubCommand::FlipLayout(arg) => {
            send_message(&SocketMessage::FlipLayout(arg.axis))?;
        }
        SubCommand::Stack(arg) => {
            send_message(&SocketMessage::StackWindow(arg.operation_direction))?;
        }
        SubCommand::StackAll => {
            send_message(&SocketMessage::StackAll)?;
        }
        SubCommand::Unstack => {
            send_message(&SocketMessage::UnstackWindow)?;
        }
        SubCommand::UnstackAll => {
            send_message(&SocketMessage::UnstackAll)?;
        }
        SubCommand::FocusStackWindow(arg) => {
            send_message(&SocketMessage::FocusStackWindow(arg.target))?;
        }
        SubCommand::CycleStack(arg) => {
            send_message(&SocketMessage::CycleStack(arg.cycle_direction))?;
        }
        SubCommand::CycleStackIndex(arg) => {
            send_message(&SocketMessage::CycleStackIndex(arg.cycle_direction))?;
        }
        SubCommand::FocusWorkspace(arg) => {
            send_message(&SocketMessage::FocusWorkspaceNumber(arg.target))?;
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
        SubCommand::Promote => {
            send_message(&SocketMessage::Promote)?;
        }
        SubCommand::PromoteFocus => {
            send_message(&SocketMessage::PromoteFocus)?;
        }
        SubCommand::PromoteWindow(arg) => {
            send_message(&SocketMessage::PromoteWindow(arg.operation_direction))?;
        }
        SubCommand::ToggleWindowBasedWorkAreaOffset => {
            send_message(&SocketMessage::ToggleWindowBasedWorkAreaOffset)?;
        }
        SubCommand::ToggleWindowContainerBehaviour => {
            send_message(&SocketMessage::ToggleWindowContainerBehaviour)?;
        }
        SubCommand::ToggleFloatOverride => {
            send_message(&SocketMessage::ToggleFloatOverride)?;
        }
        SubCommand::ToggleWorkspaceWindowContainerBehaviour => {
            send_message(&SocketMessage::ToggleWorkspaceWindowContainerBehaviour)?;
        }
        SubCommand::ToggleWorkspaceFloatOverride => {
            send_message(&SocketMessage::ToggleWorkspaceFloatOverride)?;
        }
        SubCommand::ToggleTiling => {
            send_message(&SocketMessage::ToggleTiling)?;
        }
        SubCommand::ToggleLock => {
            send_message(&SocketMessage::ToggleLock)?;
        }
        SubCommand::ToggleCrossMonitorMoveBehaviour => {
            send_message(&SocketMessage::ToggleCrossMonitorMoveBehaviour)?;
        }
        SubCommand::FocusMonitor(arg) => {
            send_message(&SocketMessage::FocusMonitorNumber(arg.target))?;
        }
        SubCommand::FocusMonitorAtCursor => {
            send_message(&SocketMessage::FocusMonitorAtCursor)?;
        }
        SubCommand::FocusLastWorkspace => {
            send_message(&SocketMessage::FocusLastWorkspace)?;
        }
        SubCommand::FocusWorkspaces(arg) => {
            send_message(&SocketMessage::FocusWorkspaceNumbers(arg.target))?;
        }
        SubCommand::FocusMonitorWorkspace(arg) => {
            send_message(&SocketMessage::FocusMonitorWorkspaceNumber(
                arg.target_monitor,
                arg.target_workspace,
            ))?;
        }
        SubCommand::FocusNamedWorkspace(arg) => {
            send_message(&SocketMessage::FocusNamedWorkspace(arg.workspace))?;
        }
        SubCommand::CloseWorkspace => {
            send_message(&SocketMessage::CloseWorkspace)?;
        }
        SubCommand::CycleMonitor(arg) => {
            send_message(&SocketMessage::CycleFocusMonitor(arg.cycle_direction))?;
        }
        SubCommand::CycleWorkspace(arg) => {
            send_message(&SocketMessage::CycleFocusWorkspace(arg.cycle_direction))?;
        }
        SubCommand::CycleEmptyWorkspace(arg) => {
            send_message(&SocketMessage::CycleFocusEmptyWorkspace(
                arg.cycle_direction,
            ))?;
        }
        SubCommand::MoveToMonitor(arg) => {
            send_message(&SocketMessage::MoveContainerToMonitorNumber(arg.target))?;
        }
        SubCommand::CycleMoveToMonitor(arg) => {
            send_message(&SocketMessage::CycleMoveContainerToMonitor(
                arg.cycle_direction,
            ))?;
        }
        SubCommand::MoveToWorkspace(arg) => {
            send_message(&SocketMessage::MoveContainerToWorkspaceNumber(arg.target))?;
        }
        SubCommand::MoveToNamedWorkspace(arg) => {
            send_message(&SocketMessage::MoveContainerToNamedWorkspace(arg.workspace))?;
        }
        SubCommand::CycleMoveToWorkspace(arg) => {
            send_message(&SocketMessage::CycleMoveContainerToWorkspace(
                arg.cycle_direction,
            ))?;
        }
        SubCommand::SendToMonitor(arg) => {
            send_message(&SocketMessage::SendContainerToMonitorNumber(arg.target))?;
        }
        SubCommand::CycleSendToMonitor(arg) => {
            send_message(&SocketMessage::CycleSendContainerToMonitor(
                arg.cycle_direction,
            ))?;
        }
        SubCommand::SendToWorkspace(arg) => {
            send_message(&SocketMessage::SendContainerToWorkspaceNumber(arg.target))?;
        }
        SubCommand::SendToNamedWorkspace(arg) => {
            send_message(&SocketMessage::SendContainerToNamedWorkspace(arg.workspace))?;
        }
        SubCommand::CycleSendToWorkspace(arg) => {
            send_message(&SocketMessage::CycleSendContainerToWorkspace(
                arg.cycle_direction,
            ))?;
        }
        SubCommand::SendToMonitorWorkspace(arg) => {
            send_message(&SocketMessage::SendContainerToMonitorWorkspaceNumber(
                arg.target_monitor,
                arg.target_workspace,
            ))?;
        }
        SubCommand::MoveToMonitorWorkspace(arg) => {
            send_message(&SocketMessage::MoveContainerToMonitorWorkspaceNumber(
                arg.target_monitor,
                arg.target_workspace,
            ))?;
        }
        SubCommand::MoveWorkspaceToMonitor(arg) => {
            send_message(&SocketMessage::MoveWorkspaceToMonitorNumber(arg.target))?;
        }
        SubCommand::CycleMoveWorkspaceToMonitor(arg) => {
            send_message(&SocketMessage::CycleMoveWorkspaceToMonitor(
                arg.cycle_direction,
            ))?;
        }
        SubCommand::MoveToLastWorkspace => {
            send_message(&SocketMessage::MoveContainerToLastWorkspace)?;
        }
        SubCommand::SendToLastWorkspace => {
            send_message(&SocketMessage::SendContainerToLastWorkspace)?;
        }
        SubCommand::SwapWorkspacesWithMonitor(arg) => {
            send_message(&SocketMessage::SwapWorkspacesToMonitorNumber(arg.target))?;
        }
        SubCommand::State => {
            print_query(&SocketMessage::State);
        }
        SubCommand::GlobalState => {
            print_query(&SocketMessage::GlobalState);
        }
        SubCommand::VisibleWindows => {
            print_query(&SocketMessage::VisibleWindows);
        }
        SubCommand::MonitorInformation => {
            print_query(&SocketMessage::MonitorInformation);
        }
        SubCommand::FetchAppSpecificConfiguration => {
            let content = reqwest::blocking::get("https://raw.githubusercontent.com/LGUG2Z/komorebi-application-specific-configuration/master/applications.mac.json")?
                .text()?;

            let output_file = HOME_DIR.join("applications.json");

            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&output_file)?;

            file.write_all(content.as_bytes())?;

            println!(
                "Latest version of applications.mac.json from https://github.com/LGUG2Z/komorebi-application-specific-configuration downloaded\n"
            );
            println!(
                "You can add this to your komorebi.json static configuration file like this: \n\n\"app_specific_configuration_path\": \"{}\"",
                output_file.display().to_string().replace("\\", "/")
            );
        }
    }

    Ok(())
}

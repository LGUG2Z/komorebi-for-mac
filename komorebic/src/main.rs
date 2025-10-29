#![warn(clippy::all)]

use chrono::Utc;
use clap::CommandFactory;
use clap::Parser;
use clap::ValueEnum;
use color_eyre::eyre;
use fs_tail::TailedFile;
use komorebi_client::ApplicationIdentifier;
use komorebi_client::Axis;
use komorebi_client::CycleDirection;
use komorebi_client::DefaultLayout;
use komorebi_client::MoveBehaviour;
use komorebi_client::OperationBehaviour;
use komorebi_client::OperationDirection;
use komorebi_client::PathExt;
use komorebi_client::Rect;
use komorebi_client::Sizing;
use komorebi_client::SocketMessage;
use komorebi_client::StateQuery;
use komorebi_client::WindowKind;
use komorebi_client::replace_env_in_path;
use komorebi_client::send_message;
use komorebi_client::send_query;
use komorebi_client::splash;
use komorebi_client::splash::ValidationFeedback;
use lazy_static::lazy_static;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use sysinfo::ProcessesToUpdate;

lazy_static! {
    static ref HAS_CUSTOM_CONFIG_HOME: AtomicBool = AtomicBool::new(false);
    static ref HOME_DIR: PathBuf = {
        std::env::var("KOMOREBI_CONFIG_HOME").map_or_else(
            |_| dirs::home_dir().expect("there is no home directory").join(".config").join("komorebi"),
            |home_path| {
                let home = home_path.replace_env();
                if home.as_path().is_dir() {
                    HAS_CUSTOM_CONFIG_HOME.store(true, Ordering::SeqCst);
                    home
                } else {
                    panic!(
                        "$KOMOREBI_CONFIG_HOME is set to \"{home_path}\", which is not a valid directory",
                    );
                }
            },
        )
    };
    static ref DATA_DIR: PathBuf = dirs::data_local_dir()
        .expect("there is no local data directory")
        .join("komorebi");
}

shadow_rs::shadow!(build);

#[derive(Copy, Clone, ValueEnum)]
enum BooleanState {
    Enable,
    Disable,
}

impl From<BooleanState> for bool {
    fn from(b: BooleanState) -> Self {
        match b {
            BooleanState::Enable => true,
            BooleanState::Disable => false,
        }
    }
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
    MouseFollowsFocus: BooleanState,
    Query: StateQuery,
    // WindowHidingBehaviour: HidingBehaviour,
    CrossMonitorMoveBehaviour: MoveBehaviour,
    UnmanagedWindowOperationBehaviour: OperationBehaviour,
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

// Thanks to @danielhenrymantilla for showing me how to use cfg_attr with an optional argument like
// this on the Rust Programming Language Community Discord Server
macro_rules! gen_workspace_subcommand_args {
    // Workspace Property: #[enum] Value Enum (if the value is an Enum)
    // Workspace Property: Value Type (if the value is anything else)
    ( $( $name:ident: $(#[enum] $(@$value_enum:tt)?)? $value:ty ),+ $(,)? ) => (
        pastey::paste! {
            $(
                #[derive(clap::Parser)]
                pub struct [<Workspace $name>] {
                    /// Monitor index (zero-indexed)
                    monitor: usize,

                    /// Workspace index on the specified monitor (zero-indexed)
                    workspace: usize,

                    $(#[clap(value_enum)] $($value_enum)?)?
                    #[cfg_attr(
                        all($(FALSE $($value_enum)?)?),
                        doc = ""$name " of the workspace as a "$value ""
                    )]
                    value: $value,
                }
            )+
        }
    )
}

gen_workspace_subcommand_args! {
    Name: String,
    Layout: #[enum] DefaultLayout,
    Tiling: #[enum] BooleanState,
}

macro_rules! gen_named_workspace_subcommand_args {
    // Workspace Property: #[enum] Value Enum (if the value is an Enum)
    // Workspace Property: Value Type (if the value is anything else)
    ( $( $name:ident: $(#[enum] $(@$value_enum:tt)?)? $value:ty ),+ $(,)? ) => (
        pastey::paste! {
            $(
                #[derive(clap::Parser)]
                pub struct [<NamedWorkspace $name>] {
                    /// Target workspace name
                    workspace: String,

                    $(#[clap(value_enum)] $($value_enum)?)?
                    #[cfg_attr(
                        all($(FALSE $($value_enum)?)?),
                        doc = ""$name " of the workspace as a "$value ""
                    )]
                    value: $value,
                }
            )+
        }
    )
}

gen_named_workspace_subcommand_args! {
    Layout: #[enum] DefaultLayout,
    Tiling: #[enum] BooleanState,
}

macro_rules! gen_focused_workspace_padding_subcommand_args {
    // SubCommand Pattern
    ( $( $name:ident ),+ $(,)? ) => {
        $(
            #[derive(clap::Parser)]
            pub struct $name {
                /// Pixels size to set as an integer
                size: i32,
            }
        )+
    };
}

gen_focused_workspace_padding_subcommand_args! {
    FocusedWorkspaceContainerPadding,
    FocusedWorkspacePadding,
}

macro_rules! gen_padding_subcommand_args {
    // SubCommand Pattern
    ( $( $name:ident ),+ $(,)? ) => {
        $(
            #[derive(clap::Parser)]
            pub struct $name {
                /// Monitor index (zero-indexed)
                monitor: usize,
                /// Workspace index on the specified monitor (zero-indexed)
                workspace: usize,
                /// Pixels to pad with as an integer
                size: i32,
            }
        )+
    };
}

gen_padding_subcommand_args! {
    ContainerPadding,
    WorkspacePadding,
}

macro_rules! gen_named_padding_subcommand_args {
    // SubCommand Pattern
    ( $( $name:ident ),+ $(,)? ) => {
        $(
            #[derive(clap::Parser)]
            pub struct $name {
                /// Target workspace name
                workspace: String,

                /// Pixels to pad with as an integer
                size: i32,
            }
        )+
    };
}

gen_named_padding_subcommand_args! {
    NamedWorkspaceContainerPadding,
    NamedWorkspacePadding,
}

macro_rules! gen_padding_adjustment_subcommand_args {
    // SubCommand Pattern
    ( $( $name:ident ),+ $(,)? ) => {
        $(
            #[derive(clap::Parser)]
            pub struct $name {
                #[clap(value_enum)]
                sizing: Sizing,
                /// Pixels to adjust by as an integer
                adjustment: i32,
            }
        )+
    };
}

gen_padding_adjustment_subcommand_args! {
    AdjustContainerPadding,
    AdjustWorkspacePadding,
}

macro_rules! gen_application_target_subcommand_args {
    // SubCommand Pattern
    ( $( $name:ident ),+ $(,)? ) => {
        $(
            #[derive(clap::Parser)]
            pub struct $name {
                #[clap(value_enum)]
                identifier: ApplicationIdentifier,
                /// Identifier as a string
                id: String,
            }
        )+
    };
}

gen_application_target_subcommand_args! {
    IgnoreRule,
    ManageRule,
    IdentifyTrayApplication,
    IdentifyLayeredApplication,
    IdentifyObjectNameChangeApplication,
    IdentifyBorderOverflowApplication,
    RemoveTitleBar,
}

#[derive(Parser)]
pub struct ClearWorkspaceLayoutRules {
    /// Monitor index (zero-indexed)
    monitor: usize,

    /// Workspace index on the specified monitor (zero-indexed)
    workspace: usize,
}

#[derive(Parser)]
struct InitialWorkspaceRule {
    #[clap(value_enum)]
    identifier: ApplicationIdentifier,
    /// Identifier as a string
    id: String,
    /// Monitor index (zero-indexed)
    monitor: usize,
    /// Workspace index on the specified monitor (zero-indexed)
    workspace: usize,
}

#[derive(Parser)]
struct InitialNamedWorkspaceRule {
    #[clap(value_enum)]
    identifier: ApplicationIdentifier,
    /// Identifier as a string
    id: String,
    /// Name of a workspace
    workspace: String,
}

#[derive(Parser)]
struct WorkspaceRule {
    #[clap(value_enum)]
    identifier: ApplicationIdentifier,
    /// Identifier as a string
    id: String,
    /// Monitor index (zero-indexed)
    monitor: usize,
    /// Workspace index on the specified monitor (zero-indexed)
    workspace: usize,
}

#[derive(Parser)]
struct NamedWorkspaceRule {
    #[clap(value_enum)]
    identifier: ApplicationIdentifier,
    /// Identifier as a string
    id: String,
    /// Name of a workspace
    workspace: String,
}

#[derive(Parser)]
struct ClearWorkspaceRules {
    /// Monitor index (zero-indexed)
    monitor: usize,
    /// Workspace index on the specified monitor (zero-indexed)
    workspace: usize,
}

#[derive(Parser)]
struct ClearNamedWorkspaceRules {
    /// Name of a workspace
    workspace: String,
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
struct ResizeDelta {
    /// The delta of pixels by which to increase or decrease window dimensions when resizing
    pixels: i32,
}

#[derive(Parser)]
struct SubscribeSocket {
    /// Name of the socket to send event notifications to
    socket: String,
}

#[derive(Parser)]
struct UnsubscribeSocket {
    /// Name of the socket to stop sending event notifications to
    socket: String,
}

#[derive(Parser)]
struct GlobalWorkAreaOffset {
    /// Size of the left work area offset (set right to left * 2 to maintain right padding)
    left: i32,
    /// Size of the top work area offset (set bottom to the same value to maintain bottom padding)
    top: i32,
    /// Size of the right work area offset
    right: i32,
    /// Size of the bottom work area offset
    bottom: i32,
}

#[derive(Parser)]
struct MonitorWorkAreaOffset {
    /// Monitor index (zero-indexed)
    monitor: usize,
    /// Size of the left work area offset (set right to left * 2 to maintain right padding)
    left: i32,
    /// Size of the top work area offset (set bottom to the same value to maintain bottom padding)
    top: i32,
    /// Size of the right work area offset
    right: i32,
    /// Size of the bottom work area offset
    bottom: i32,
}

#[derive(Parser)]
struct WorkspaceWorkAreaOffset {
    /// Monitor index (zero-indexed)
    monitor: usize,
    /// Workspace index (zero-indexed)
    workspace: usize,
    /// Size of the left work area offset (set right to left * 2 to maintain right padding)
    left: i32,
    /// Size of the top work area offset (set bottom to the same value to maintain bottom padding)
    top: i32,
    /// Size of the right work area offset
    right: i32,
    /// Size of the bottom work area offset
    bottom: i32,
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
pub struct WorkspaceLayoutRule {
    /// Monitor index (zero-indexed)
    monitor: usize,

    /// Workspace index on the specified monitor (zero-indexed)
    workspace: usize,

    /// The number of window containers on-screen required to trigger this layout rule
    at_container_count: usize,

    #[clap(value_enum)]
    layout: DefaultLayout,
}

#[derive(Parser)]
pub struct NamedWorkspaceLayoutRule {
    /// Target workspace name
    workspace: String,

    /// The number of window containers on-screen required to trigger this layout rule
    at_container_count: usize,

    #[clap(value_enum)]
    layout: DefaultLayout,
}

#[derive(Parser)]
struct DisplayIndexPreference {
    /// Preferred monitor index (zero-indexed)
    index_preference: usize,
    /// Display name as identified in komorebic state
    display: String,
}

#[derive(Parser)]
struct EnsureWorkspaces {
    /// Monitor index (zero-indexed)
    monitor: usize,
    /// Number of desired workspaces
    workspace_count: usize,
}

#[derive(Parser)]
struct EnsureNamedWorkspaces {
    /// Monitor index (zero-indexed)
    monitor: usize,
    /// Names of desired workspaces
    names: Vec<String>,
}

#[derive(Parser)]
#[allow(clippy::struct_excessive_bools)]
struct Start {
    /// Path to a static configuration JSON file
    #[clap(short, long)]
    #[clap(value_parser = replace_env_in_path)]
    config: Option<PathBuf>,
    /// Start komorebi-bar in a background process
    #[clap(long)]
    bar: bool,
    // /// Do not attempt to auto-apply a dumped state temp file from a previously running instance of komorebi
    // #[clap(long)]
    // clean_state: bool,
}

#[derive(Parser)]
struct Stop {
    /// Do not restore windows after stopping komorebi
    #[clap(long, hide = true)]
    ignore_restore: bool,
    /// Stop komorebi-bar if it is running as a background process
    #[clap(long)]
    bar: bool,
}

#[derive(Parser)]
struct SaveResize {
    /// File to which the resize layout dimensions should be saved
    #[clap(value_parser = replace_env_in_path)]
    path: PathBuf,
}

#[derive(Parser)]
struct LoadResize {
    /// File from which the resize layout dimensions should be loaded
    #[clap(value_parser = replace_env_in_path)]
    path: PathBuf,
}

#[derive(Parser)]
struct ReplaceConfiguration {
    /// Static configuration JSON file from which the configuration should be loaded
    #[clap(value_parser = replace_env_in_path)]
    path: PathBuf,
}

#[derive(Parser)]
struct EnableAutostart {
    /// Path to a static configuration JSON file
    #[clap(action, short, long)]
    #[clap(value_parser = replace_env_in_path)]
    config: Option<PathBuf>,
}

#[derive(Parser)]
struct EagerFocus {
    /// Case-sensitive exe identifier
    exe: String,
}

#[derive(Parser)]
struct ScrollingLayoutColumns {
    /// Desired number of visible columns
    count: NonZeroUsize,
}

#[derive(Parser)]
struct Border {
    #[clap(value_enum)]
    boolean_state: BooleanState,
}

#[derive(Parser)]
struct BorderColour {
    #[clap(value_enum, short, long, default_value = "single")]
    window_kind: WindowKind,
    /// Red
    r: u32,
    /// Green
    g: u32,
    /// Blue
    b: u32,
}

#[derive(Parser)]
struct BorderWidth {
    /// Desired width of the window border
    width: i32,
}

#[derive(Parser)]
struct BorderOffset {
    /// Desired offset of the window border
    offset: i32,
}

#[derive(Parser)]
struct Completions {
    #[clap(value_enum)]
    shell: clap_complete::Shell,
}

#[derive(Parser)]
struct License {
    /// Email address associated with an Individual Commercial Use License
    email: String,
}

#[derive(Parser)]
struct Splash {
    mdm_server: Option<String>,
}

#[derive(Parser)]
#[clap(author, about, version = build::CLAP_LONG_VERSION)]
struct Opts {
    #[clap(subcommand)]
    subcmd: SubCommand,
}

#[derive(Parser)]
enum SubCommand {
    #[clap(hide = true)]
    Docgen,
    #[clap(hide = true)]
    Splash(Splash),
    /// Generate komorebic CLI completions for the target shell
    #[clap(arg_required_else_help = true)]
    Completions(Completions),
    /// Gather example configurations for a new-user quickstart
    Quickstart,
    /// Specify an email associated with an Individual Commercial Use License
    #[clap(arg_required_else_help = true)]
    License(License),
    /// Start komorebi as a background process
    Start(Start),
    /// Stop the komorebi process and restore all hidden windows
    Stop(Stop),
    // /// Kill background processes started by komorebic
    // Kill(Kill),
    // /// Check komorebi configuration and related files for common errors
    // Check(Check),
    /// Show the path to komorebi.json
    #[clap(alias = "config")]
    Configuration,
    // /// Show the path to komorebi.bar.json
    // #[clap(alias = "bar-config")]
    // #[clap(alias = "bconfig")]
    // BarConfiguration,
    // /// Show the path to whkdrc
    // #[clap(alias = "whkd")]
    // Whkdrc,
    /// Show the path to komorebi's data directory in $HOME/Library/Application Support
    #[clap(alias = "datadir")]
    DataDirectory,
    /// Show a JSON representation of the current window manager state
    State,
    /// Show a JSON representation of the current global state
    GlobalState,
    // /// Launch the komorebi-gui debugging tool
    // Gui,
    // /// Toggle the komorebi-shortcuts helper
    // ToggleShortcuts,
    /// Show a JSON representation of visible windows
    VisibleWindows,
    /// Show information about connected monitors
    #[clap(alias = "monitor-info")]
    MonitorInformation,
    /// Query the current window manager state
    #[clap(arg_required_else_help = true)]
    Query(Query),
    /// Subscribe to komorebi events using a Unix Domain Socket
    #[clap(arg_required_else_help = true)]
    SubscribeSocket(SubscribeSocket),
    /// Unsubscribe from komorebi events
    #[clap(arg_required_else_help = true)]
    UnsubscribeSocket(UnsubscribeSocket),
    // /// Subscribe to komorebi events using a Named Pipe
    // #[clap(arg_required_else_help = true)]
    // #[clap(alias = "subscribe")]
    // SubscribePipe(SubscribePipe),
    // /// Unsubscribe from komorebi events
    // #[clap(arg_required_else_help = true)]
    // #[clap(alias = "unsubscribe")]
    // UnsubscribePipe(UnsubscribePipe),
    /// Tail komorebi's process logs (cancel with Ctrl-C)
    Log,
    /// Quicksave the current resize layout dimensions
    #[clap(alias = "quick-save")]
    QuickSaveResize,
    /// Load the last quicksaved resize layout dimensions
    #[clap(alias = "quick-load")]
    QuickLoadResize,
    /// Save the current resize layout dimensions to a file
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "save")]
    SaveResize(SaveResize),
    /// Load the resize layout dimensions from a file
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "load")]
    LoadResize(LoadResize),
    /// Change focus to the window in the specified direction
    #[clap(arg_required_else_help = true)]
    Focus(Focus),
    /// Move the focused window in the specified direction
    #[clap(arg_required_else_help = true)]
    Move(Move),
    /// Minimize the focused window
    Minimize,
    /// Close the focused window
    Close,
    // /// Forcibly focus the window at the cursor with a left mouse click
    // ForceFocus,
    /// Change focus to the window in the specified cycle direction
    #[clap(arg_required_else_help = true)]
    CycleFocus(CycleFocus),
    /// Move the focused window in the specified cycle direction
    #[clap(arg_required_else_help = true)]
    CycleMove(CycleMove),
    /// Focus the first managed window matching the given executable
    #[clap(arg_required_else_help = true)]
    EagerFocus(EagerFocus),
    /// Stack the focused window in the specified direction
    #[clap(arg_required_else_help = true)]
    Stack(Stack),
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
    CycleEmptyWorkspace(CycleWorkspace),
    /// Move the focused workspace to the specified monitor
    #[clap(arg_required_else_help = true)]
    MoveWorkspaceToMonitor(MoveWorkspaceToMonitor),
    /// Move the focused workspace monitor in the given cycle direction
    #[clap(arg_required_else_help = true)]
    CycleMoveWorkspaceToMonitor(CycleMoveWorkspaceToMonitor),
    /// Swap focused monitor workspaces with specified monitor
    #[clap(arg_required_else_help = true)]
    SwapWorkspacesWithMonitor(SwapWorkspacesWithMonitor),
    /// Create and append a new workspace on the focused monitor
    NewWorkspace,
    /// Set the resize delta (used by resize-edge and resize-axis)
    #[clap(arg_required_else_help = true)]
    ResizeDelta(ResizeDelta),
    // /// Set the invisible border dimensions around each window
    // #[clap(arg_required_else_help = true)]
    // InvisibleBorders(InvisibleBorders),
    /// Set offsets to exclude parts of the work area from tiling
    #[clap(arg_required_else_help = true)]
    GlobalWorkAreaOffset(GlobalWorkAreaOffset),
    /// Set offsets for a monitor to exclude parts of the work area from tiling
    #[clap(arg_required_else_help = true)]
    MonitorWorkAreaOffset(MonitorWorkAreaOffset),
    /// Set offsets for a workspace to exclude parts of the work area from tiling
    #[clap(arg_required_else_help = true)]
    WorkspaceWorkAreaOffset(WorkspaceWorkAreaOffset),
    /// Toggle application of the window-based work area offset for the focused workspace
    ToggleWindowBasedWorkAreaOffset,
    /// Set container padding on the focused workspace
    #[clap(arg_required_else_help = true)]
    FocusedWorkspaceContainerPadding(FocusedWorkspaceContainerPadding),
    /// Set workspace padding on the focused workspace
    #[clap(arg_required_else_help = true)]
    FocusedWorkspacePadding(FocusedWorkspacePadding),
    /// Adjust container padding on the focused workspace
    #[clap(arg_required_else_help = true)]
    AdjustContainerPadding(AdjustContainerPadding),
    /// Adjust workspace padding on the focused workspace
    #[clap(arg_required_else_help = true)]
    AdjustWorkspacePadding(AdjustWorkspacePadding),
    /// Set the layout on the focused workspace
    #[clap(arg_required_else_help = true)]
    ChangeLayout(ChangeLayout),
    /// Cycle between available layouts
    #[clap(arg_required_else_help = true)]
    CycleLayout(CycleLayout),
    /// Set the number of visible columns for the Scrolling layout on the focused workspace
    #[clap(arg_required_else_help = true)]
    ScrollingLayoutColumns(ScrollingLayoutColumns),
    // /// Load a custom layout from file for the focused workspace
    // #[clap(hide = true)]
    // #[clap(arg_required_else_help = true)]
    // LoadCustomLayout(LoadCustomLayout),
    /// Flip the layout on the focused workspace
    #[clap(arg_required_else_help = true)]
    FlipLayout(FlipLayout),
    /// Promote the focused window to the top of the tree
    Promote,
    /// Promote the user focus to the top of the tree
    PromoteFocus,
    /// Promote the window in the specified direction
    PromoteWindow(PromoteWindow),
    /// Force the retiling of all managed windows
    Retile,
    // /// Set the monitor index preference for a monitor identified using its size
    // #[clap(arg_required_else_help = true)]
    // MonitorIndexPreference(MonitorIndexPreference),
    /// Set the display index preference for a monitor identified using its display name
    #[clap(arg_required_else_help = true)]
    DisplayIndexPreference(DisplayIndexPreference),
    /// Create at least this many workspaces for the specified monitor
    #[clap(arg_required_else_help = true)]
    EnsureWorkspaces(EnsureWorkspaces),
    /// Create these many named workspaces for the specified monitor
    #[clap(arg_required_else_help = true)]
    EnsureNamedWorkspaces(EnsureNamedWorkspaces),
    /// Set the container padding for the specified workspace
    #[clap(arg_required_else_help = true)]
    ContainerPadding(ContainerPadding),
    /// Set the container padding for the specified workspace
    #[clap(arg_required_else_help = true)]
    NamedWorkspaceContainerPadding(NamedWorkspaceContainerPadding),
    /// Set the workspace padding for the specified workspace
    #[clap(arg_required_else_help = true)]
    WorkspacePadding(WorkspacePadding),
    /// Set the workspace padding for the specified workspace
    #[clap(arg_required_else_help = true)]
    NamedWorkspacePadding(NamedWorkspacePadding),
    /// Set the layout for the specified workspace
    #[clap(arg_required_else_help = true)]
    WorkspaceLayout(WorkspaceLayout),
    /// Set the layout for the specified workspace
    #[clap(arg_required_else_help = true)]
    NamedWorkspaceLayout(NamedWorkspaceLayout),
    // /// Set a custom layout for the specified workspace
    // #[clap(hide = true)]
    // #[clap(arg_required_else_help = true)]
    // WorkspaceCustomLayout(WorkspaceCustomLayout),
    // /// Set a custom layout for the specified workspace
    // #[clap(hide = true)]
    // #[clap(arg_required_else_help = true)]
    // NamedWorkspaceCustomLayout(NamedWorkspaceCustomLayout),
    /// Add a dynamic layout rule for the specified workspace
    #[clap(arg_required_else_help = true)]
    WorkspaceLayoutRule(WorkspaceLayoutRule),
    /// Add a dynamic layout rule for the specified workspace
    #[clap(arg_required_else_help = true)]
    NamedWorkspaceLayoutRule(NamedWorkspaceLayoutRule),
    // /// Add a dynamic custom layout for the specified workspace
    // #[clap(hide = true)]
    // #[clap(arg_required_else_help = true)]
    // WorkspaceCustomLayoutRule(WorkspaceCustomLayoutRule),
    // /// Add a dynamic custom layout for the specified workspace
    // #[clap(hide = true)]
    // #[clap(arg_required_else_help = true)]
    // NamedWorkspaceCustomLayoutRule(NamedWorkspaceCustomLayoutRule),
    /// Clear all dynamic layout rules for the specified workspace
    #[clap(arg_required_else_help = true)]
    ClearWorkspaceLayoutRules(ClearWorkspaceLayoutRules),
    /// Clear all dynamic layout rules for the specified workspace
    #[clap(arg_required_else_help = true)]
    ClearNamedWorkspaceLayoutRules(ClearNamedWorkspaceLayoutRules),
    /// Enable or disable window tiling for the specified workspace
    #[clap(arg_required_else_help = true)]
    WorkspaceTiling(WorkspaceTiling),
    /// Enable or disable window tiling for the specified workspace
    #[clap(arg_required_else_help = true)]
    NamedWorkspaceTiling(NamedWorkspaceTiling),
    /// Set the workspace name for the specified workspace
    #[clap(arg_required_else_help = true)]
    WorkspaceName(WorkspaceName),
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
    /// Toggle between the Tiling and Floating layers on the focused workspace
    ToggleWorkspaceLayer,
    /// Toggle the paused state for all window tiling
    TogglePause,
    /// Toggle window tiling on the focused workspace
    ToggleTiling,
    /// Toggle floating mode for the focused window
    ToggleFloat,
    /// Toggle monocle mode for the focused container
    ToggleMonocle,
    // /// Toggle native maximization for the focused window
    // ToggleMaximize,
    /// Toggle a lock for the focused container, ensuring it will not be displaced by any new windows
    ToggleLock,
    // /// Restore all hidden windows (debugging command)
    // RestoreWindows,
    /// Force komorebi to manage the focused window
    Manage,
    /// Unmanage a window that was forcibly managed
    Unmanage,
    /// Replace the configuration of a running instance of komorebi from a static configuration file
    #[clap(arg_required_else_help = true)]
    ReplaceConfiguration(ReplaceConfiguration),
    // /// Reload legacy komorebi.ahk or komorebi.ps1 configurations (if they exist)
    // ReloadConfiguration,
    // /// Enable or disable watching of legacy komorebi.ahk or komorebi.ps1 configurations (if they exist)
    // #[clap(arg_required_else_help = true)]
    // WatchConfiguration(WatchConfiguration),
    // /// For legacy komorebi.ahk or komorebi.ps1 configurations, signal that the final configuration option has been sent
    // CompleteConfiguration,
    // /// DEPRECATED since v0.1.22
    // #[clap(arg_required_else_help = true)]
    // #[clap(hide = true)]
    // AltFocusHack(AltFocusHack),
    // /// Set the window behaviour when switching workspaces / cycling stacks
    // #[clap(arg_required_else_help = true)]
    // WindowHidingBehaviour(WindowHidingBehaviour),
    /// Set the behaviour when moving windows across monitor boundaries
    #[clap(arg_required_else_help = true)]
    CrossMonitorMoveBehaviour(CrossMonitorMoveBehaviour),
    /// Toggle the behaviour when moving windows across monitor boundaries
    ToggleCrossMonitorMoveBehaviour,
    /// Set the operation behaviour when the focused window is not managed
    #[clap(arg_required_else_help = true)]
    UnmanagedWindowOperationBehaviour(UnmanagedWindowOperationBehaviour),
    /// Add a rule to float the foreground window for the rest of this session
    SessionFloatRule,
    /// Show all session float rules
    SessionFloatRules,
    /// Clear all session float rules
    ClearSessionFloatRules,
    /// Add a rule to ignore the specified application
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "float-rule")]
    IgnoreRule(IgnoreRule),
    /// Add a rule to always manage the specified application
    #[clap(arg_required_else_help = true)]
    ManageRule(ManageRule),
    /// Add a rule to associate an application with a workspace on first show
    #[clap(arg_required_else_help = true)]
    InitialWorkspaceRule(InitialWorkspaceRule),
    /// Add a rule to associate an application with a named workspace on first show
    #[clap(arg_required_else_help = true)]
    InitialNamedWorkspaceRule(InitialNamedWorkspaceRule),
    /// Add a rule to associate an application with a workspace
    #[clap(arg_required_else_help = true)]
    WorkspaceRule(WorkspaceRule),
    /// Add a rule to associate an application with a named workspace
    #[clap(arg_required_else_help = true)]
    NamedWorkspaceRule(NamedWorkspaceRule),
    /// Remove all application association rules for a workspace by monitor and workspace index
    #[clap(arg_required_else_help = true)]
    ClearWorkspaceRules(ClearWorkspaceRules),
    /// Remove all application association rules for a named workspace
    #[clap(arg_required_else_help = true)]
    ClearNamedWorkspaceRules(ClearNamedWorkspaceRules),
    /// Remove all application association rules for all workspaces
    ClearAllWorkspaceRules,
    /// Enforce all workspace rules, including initial workspace rules that have already been applied
    EnforceWorkspaceRules,
    // /// Identify an application that sends EVENT_OBJECT_NAMECHANGE on launch
    // #[clap(arg_required_else_help = true)]
    // IdentifyObjectNameChangeApplication(IdentifyObjectNameChangeApplication),
    // /// Identify an application that closes to the system tray
    // #[clap(arg_required_else_help = true)]
    // IdentifyTrayApplication(IdentifyTrayApplication),
    // /// Identify an application that has WS_EX_LAYERED, but should still be managed
    // #[clap(arg_required_else_help = true)]
    // IdentifyLayeredApplication(IdentifyLayeredApplication),
    // /// Whitelist an application for title bar removal
    // #[clap(arg_required_else_help = true)]
    // RemoveTitleBar(RemoveTitleBar),
    // /// Toggle title bars for whitelisted applications
    // ToggleTitleBars,
    // /// Identify an application that has overflowing borders
    // #[clap(hide = true)]
    // #[clap(alias = "identify-border-overflow")]
    // IdentifyBorderOverflowApplication(IdentifyBorderOverflowApplication),
    /// Enable or disable borders
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "active-window-border")]
    Border(Border),
    /// Set the colour for a window border kind
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "active-window-border-colour")]
    BorderColour(BorderColour),
    /// Set the border width
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "active-window-border-width")]
    BorderWidth(BorderWidth),
    /// Set the border offset
    #[clap(arg_required_else_help = true)]
    #[clap(alias = "active-window-border-offset")]
    BorderOffset(BorderOffset),
    // /// Set the border style
    // #[clap(arg_required_else_help = true)]
    // BorderStyle(BorderStyle),
    // /// Set the border implementation
    // #[clap(arg_required_else_help = true)]
    // BorderImplementation(BorderImplementation),
    // /// Set the stackbar mode
    // #[clap(arg_required_else_help = true)]
    // StackbarMode(StackbarMode),
    // /// Enable or disable transparency for unfocused windows
    // #[clap(arg_required_else_help = true)]
    // Transparency(Transparency),
    // /// Set the alpha value for unfocused window transparency
    // #[clap(arg_required_else_help = true)]
    // TransparencyAlpha(TransparencyAlpha),
    // /// Toggle transparency for unfocused windows
    // ToggleTransparency,
    // /// Enable or disable movement animations
    // #[clap(arg_required_else_help = true)]
    // Animation(Animation),
    // /// Set the duration for movement animations in ms
    // #[clap(arg_required_else_help = true)]
    // AnimationDuration(AnimationDuration),
    // /// Set the frames per second for movement animations
    // #[clap(arg_required_else_help = true)]
    // AnimationFps(AnimationFps),
    // /// Set the ease function for movement animations
    // #[clap(arg_required_else_help = true)]
    // AnimationStyle(AnimationStyle),
    // /// Enable or disable focus follows mouse for the operating system
    // #[clap(hide = true)]
    // #[clap(arg_required_else_help = true)]
    // FocusFollowsMouse(FocusFollowsMouse),
    // /// Toggle focus follows mouse for the operating system
    // #[clap(hide = true)]
    // #[clap(arg_required_else_help = true)]
    // ToggleFocusFollowsMouse(ToggleFocusFollowsMouse),
    /// Enable or disable mouse follows focus on all workspaces
    #[clap(arg_required_else_help = true)]
    MouseFollowsFocus(MouseFollowsFocus),
    /// Toggle mouse follows focus on all workspaces
    ToggleMouseFollowsFocus,
    // /// Generate common app-specific configurations and fixes to use in komorebi.ahk
    // #[clap(arg_required_else_help = true)]
    // #[clap(alias = "ahk-asc")]
    // AhkAppSpecificConfiguration(AhkAppSpecificConfiguration),
    // /// Generate common app-specific configurations and fixes in a PowerShell script
    // #[clap(arg_required_else_help = true)]
    // #[clap(alias = "pwsh-asc")]
    // PwshAppSpecificConfiguration(PwshAppSpecificConfiguration),
    // /// Convert a v1 ASC YAML file to a v2 ASC JSON file
    // #[clap(arg_required_else_help = true)]
    // #[clap(alias = "convert-asc")]
    // ConvertAppSpecificConfiguration(ConvertAppSpecificConfiguration),
    // /// Format a YAML file for use with the 'app-specific-configuration' command
    // #[clap(arg_required_else_help = true)]
    // #[clap(alias = "fmt-asc")]
    // #[clap(hide = true)]
    // FormatAppSpecificConfiguration(FormatAppSpecificConfiguration),
    /// Fetch the latest version of applications.json from komorebi-application-specific-configuration
    #[clap(alias = "fetch-asc")]
    FetchAppSpecificConfiguration,
    /// Generate a JSON Schema for applications.json
    #[clap(alias = "asc-schema")]
    ApplicationSpecificConfigurationSchema,
    /// Generate a JSON Schema of subscription notifications
    NotificationSchema,
    /// Generate a JSON Schema of socket messages
    SocketSchema,
    /// Generate a JSON Schema of the static configuration file
    StaticConfigSchema,
    /// Generates a static configuration JSON file based on the current window manager state
    GenerateStaticConfig,
    /// Generates and loads the com.lgug2z.komorebi.plist file in $HOME/Library/LaunchAgents
    EnableAutostart(EnableAutostart),
    /// Unloads and deletes the com.lgug2z.komorebi.plist file in $HOME/Library/LaunchAgents
    DisableAutostart,
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
        SubCommand::Docgen => {
            let mut cli = Opts::command();
            let subcommands = cli.get_subcommands_mut();
            std::fs::create_dir_all("docs/cli")?;

            let ignore = [
                "docgen",
                "alt-focus-hack",
                "identify-border-overflow-application",
                "load-custom-layout",
                "workspace-custom-layout",
                "named-workspace-custom-layout",
                "workspace-custom-layout-rule",
                "named-workspace-custom-layout-rule",
                "focus-follows-mouse",
                "toggle-focus-follows-mouse",
                "format-app-specific-configuration",
            ];

            for cmd in subcommands {
                let name = cmd.get_name().to_string();
                if !ignore.contains(&name.as_str()) {
                    let help_text = cmd.render_long_help().to_string();
                    let outpath = format!("docs/cli/{name}.md");
                    let markdown = format!("# {name}\n\n```\n{help_text}\n```");
                    std::fs::write(outpath, markdown)?;
                    println!("    - cli/{name}.md");
                }
            }
        }
        SubCommand::Splash(arg) => {
            splash::show(arg.mdm_server)?;
        }
        SubCommand::Completions(arg) => {
            let mut cli = Opts::command();
            clap_complete::generate(arg.shell, &mut cli, "komorebic", &mut std::io::stdout());
        }
        SubCommand::License(arg) => {
            let _ = std::fs::remove_file(DATA_DIR.join("icul.validation"));
            std::fs::write(DATA_DIR.join("icul"), &arg.email)?;
            match splash::should()? {
                ValidationFeedback::Successful(icul_validation) => {
                    println!("Individual commercial use license validation successful");
                    println!(
                        "Local validation file saved to {}",
                        icul_validation.display()
                    );
                    println!("\n{}", std::fs::read_to_string(&icul_validation)?);
                }
                ValidationFeedback::Unsuccessful(invalid_payload) => {
                    println!(
                        "No active individual commercial use license found for {}",
                        arg.email
                    );
                    println!("\n{invalid_payload}");
                    println!(
                        "\nYou can purchase an individual commercial use license at https://lgug2z.com/software/komorebi"
                    );
                }
                ValidationFeedback::NoEmail => {}
                ValidationFeedback::NoConnectivity => {
                    println!(
                        "Could not make a connection to validate an individual commercial use license for {}",
                        arg.email
                    );
                }
            }
        }
        SubCommand::Quickstart => {
            fn write_file_with_prompt(
                path: &PathBuf,
                content: &str,
                created_files: &mut Vec<String>,
            ) -> eyre::Result<()> {
                if path.exists() {
                    print!(
                        "{} will be overwritten, do you want to continue? (y/N): ",
                        path.display()
                    );
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    let trimmed = input.trim().to_lowercase();
                    if trimmed == "y" || trimmed == "yes" {
                        std::fs::write(path, content)?;
                        created_files.push(path.display().to_string());
                    } else {
                        println!("Skipping {}", path.display());
                    }
                } else {
                    std::fs::write(path, content)?;
                    created_files.push(path.display().to_string());
                }
                Ok(())
            }

            let local_appdata_dir = dirs::data_local_dir().expect("could not find localdata dir");
            let data_dir = local_appdata_dir.join("komorebi");
            std::fs::create_dir_all(&*HOME_DIR)?;
            std::fs::create_dir_all(data_dir)?;

            let mut komorebi_json = include_str!("../../docs/komorebi.example.json").to_string();
            let komorebi_bar_json =
                include_str!("../../docs/komorebi.bar.example.json").to_string();

            if std::env::var("KOMOREBI_CONFIG_HOME").is_ok() {
                komorebi_json =
                    komorebi_json.replace("Env:USERPROFILE", "Env:KOMOREBI_CONFIG_HOME");
            }

            let komorebi_path = HOME_DIR.join("komorebi.json");
            let bar_path = HOME_DIR.join("komorebi.bar.json");
            let applications_path = HOME_DIR.join("applications.json");
            let skhdrc_path = HOME_DIR.join("skhdrc");

            let mut written_files = Vec::new();

            write_file_with_prompt(&komorebi_path, &komorebi_json, &mut written_files)?;
            write_file_with_prompt(&bar_path, &komorebi_bar_json, &mut written_files)?;

            let applications_json = include_str!("../applications.json");
            write_file_with_prompt(&applications_path, applications_json, &mut written_files)?;

            let whkdrc = include_str!("../../docs/skhdrc.sample");
            write_file_with_prompt(&skhdrc_path, whkdrc, &mut written_files)?;
            if written_files.is_empty() {
                println!("\nNo files were written.")
            } else {
                println!(
                    "\nThe following example files were written:\n{}",
                    written_files.join("\n")
                );
            }

            let mut current_exe = std::env::current_exe().expect("unable to get exec path");
            current_exe.pop();
            let komorebi_exe = current_exe.join("komorebi");

            let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();

            println!(
                "\nPlease grant Accessibility permissions to \"{}\" and \"{term_program}\"",
                komorebi_exe.display()
            );
            println!("Accessibility permissions are required for komorebi to to manage windows");
            Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .spawn()?;

            println!("\nHit return when done...");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            println!(
                "Please grant Screen Recording permissions to \"{}\" and \"{term_program}\"",
                komorebi_exe.display()
            );
            println!(
                "Screen Recording permissions are required to for komorebi to read window titles"
            );
            Command::new("open")
                .arg(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
                )
                .spawn()?;

            println!("\nHit return when done...");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;

            if let Ok((mdm, server)) = splash::mdm_enrollment()
                && mdm
            {
                if let Some(server) = server {
                    println!(
                        "\nIt looks like you are using a corporate device enrolled in mobile device management ({server})"
                    );
                } else {
                    println!(
                        "\nIt looks like you are using a corporate device enrolled in mobile device management"
                    );
                }
                println!("The Komorebi License does not permit any kind of commercial use");
                println!(
                    "A dedicated Individual Commercial Use License is available if you wish to use this software at work"
                );
                println!(
                    "You are strongly encouraged to make your employer pay for your license, either directly or via reimbursement"
                );
                println!(
                    "If you already have a license, you can run \"komorebic license <email>\" with the email address your license is associated with"
                );
            }

            println!("You can now run: komorebic start");
            println!(
                "\nIf you have skhd hotkey daemon installed, you can also run: skhd --config {}",
                skhdrc_path.display()
            );
        }
        SubCommand::EnableAutostart(args) => {
            let mut current_exe = std::env::current_exe().expect("unable to get exec path");
            current_exe.pop();
            let komorebic_exe = current_exe.join("komorebi");

            let plist = match args.config {
                None => {
                    format!(
                        r#"
           <?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
        <key>EnvironmentVariables</key>
        <dict>
                <key>PATH</key>
                <string>$HOME/.nix-profile/bin:/etc/profiles/per-user/$USER/bin:/run/current-system/sw/bin:/nix/var/nix/profiles/default/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
        </dict>
        <key>KeepAlive</key>
        <true/>
        <key>Label</key>
        <string>com.lgug2z.komorebi</string>
        <key>ProcessType</key>
        <string>Interactive</string>
        <key>ProgramArguments</key>
        <array>
                <string>{}</string>
        </array>
</dict>
</plist>"#,
                        komorebic_exe.to_string_lossy()
                    )
                }
                Some(config) => {
                    format!(
                        r#"
           <?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
        <key>EnvironmentVariables</key>
        <dict>
                <key>PATH</key>
                <string>$HOME/.nix-profile/bin:/etc/profiles/per-user/$USER/bin:/run/current-system/sw/bin:/nix/var/nix/profiles/default/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
        </dict>
        <key>KeepAlive</key>
        <true/>
        <key>Label</key>
        <string>com.lgug2z.komorebi</string>
        <key>ProcessType</key>
        <string>Interactive</string>
        <key>ProgramArguments</key>
        <array>
                <string>{}</string>
                <string>--config</string>
                <string>{}</string>
        </array>
</dict>
</plist>"#,
                        komorebic_exe.to_string_lossy(),
                        config.to_string_lossy()
                    )
                }
            };

            let target = dirs::home_dir()
                .expect("no $HOME dir")
                .join("Library")
                .join("LaunchAgents")
                .join("com.lgug2z.komorebi.plist");

            std::fs::write(&target, &plist)?;

            println!("Created com.lgug2z.komorebi.plist at {}", target.display());
            Command::new("launchctl")
                .args(["load", &*target.to_string_lossy()])
                .output()?;
            println!("Ran 'launchctl load {}'", target.display());
        }
        SubCommand::DisableAutostart => {
            let target = dirs::home_dir()
                .expect("no $HOME dir")
                .join("Library")
                .join("LaunchAgents")
                .join("com.lgug2z.komorebi.plist");

            Command::new("launchctl")
                .args(["unload", &*target.to_string_lossy()])
                .output()?;
            println!("Ran 'launchctl unload {}'", target.display());
            std::fs::remove_file(&target)?;
            println!("Deleted {}", target.display());
        }
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

                print!("Waiting for komorebi to start... ");
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

            if arg.bar {
                let mut command = &mut Command::new("komorebi-bar");

                command = command
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .stdin(Stdio::null());

                command.spawn()?;
            }

            println!("\nThank you for using komorebi!\n");
            println!("# Commercial Use License");
            if let Ok((mdm, server)) = splash::mdm_enrollment()
                && mdm
            {
                if let Some(server) = server {
                    println!(
                        "* It looks like you are using a corporate device enrolled in mobile device management ({server})"
                    );
                } else {
                    println!(
                        "* It looks like you are using a corporate device enrolled in mobile device management"
                    );
                }
            }
            println!(
                "* View licensing options https://lgug2z.com/software/komorebi - A commercial use license is required to use komorebi at work"
            );
            println!("\n# Personal Use Sponsorship");
            println!(
                "* Become a sponsor https://github.com/sponsors/LGUG2Z - $10/month makes a big difference"
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
        SubCommand::Stop(arg) => {
            if arg.ignore_restore {
                send_message(&SocketMessage::StopIgnoreRestore)?;
            } else {
                send_message(&SocketMessage::Stop)?;
            }

            if arg.bar {
                Command::new("pkill").arg("komorebi-bar").spawn()?;
            }

            // TODO: see if we need a force quit sometimes like we do on Windows
            // let mut system = sysinfo::System::new_all();
            // system.refresh_processes(ProcessesToUpdate::All, true);
            //
            // let running = system.processes_by_exact_name("komorebi".as_ref()).next();
            //
            // if let Some(process) = running {
            //     process.kill_with(Signal::Interrupt);
            // }
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
        SubCommand::Close => {
            send_message(&SocketMessage::Close)?;
        }
        SubCommand::Minimize => {
            send_message(&SocketMessage::Minimize)?;
        }
        SubCommand::Manage => {
            send_message(&SocketMessage::ManageFocusedWindow)?;
        }
        SubCommand::Unmanage => {
            send_message(&SocketMessage::UnmanageFocusedWindow)?;
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
        SubCommand::Query(arg) => {
            print_query(&SocketMessage::Query(arg.state_query));
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
        SubCommand::SessionFloatRule => {
            send_message(&SocketMessage::SessionFloatRule)?;
        }
        SubCommand::SessionFloatRules => {
            print_query(&SocketMessage::SessionFloatRules);
        }
        SubCommand::ClearSessionFloatRules => {
            send_message(&SocketMessage::ClearSessionFloatRules)?;
        }
        SubCommand::IgnoreRule(arg) => {
            send_message(&SocketMessage::IgnoreRule(arg.identifier, arg.id))?;
        }
        SubCommand::ManageRule(arg) => {
            send_message(&SocketMessage::ManageRule(arg.identifier, arg.id))?;
        }
        SubCommand::InitialWorkspaceRule(arg) => {
            send_message(&SocketMessage::InitialWorkspaceRule(
                arg.identifier,
                arg.id,
                arg.monitor,
                arg.workspace,
            ))?;
        }
        SubCommand::InitialNamedWorkspaceRule(arg) => {
            send_message(&SocketMessage::InitialNamedWorkspaceRule(
                arg.identifier,
                arg.id,
                arg.workspace,
            ))?;
        }
        SubCommand::WorkspaceRule(arg) => {
            send_message(&SocketMessage::WorkspaceRule(
                arg.identifier,
                arg.id,
                arg.monitor,
                arg.workspace,
            ))?;
        }
        SubCommand::NamedWorkspaceRule(arg) => {
            send_message(&SocketMessage::NamedWorkspaceRule(
                arg.identifier,
                arg.id,
                arg.workspace,
            ))?;
        }
        SubCommand::ClearWorkspaceRules(arg) => {
            send_message(&SocketMessage::ClearWorkspaceRules(
                arg.monitor,
                arg.workspace,
            ))?;
        }
        SubCommand::ClearNamedWorkspaceRules(arg) => {
            send_message(&SocketMessage::ClearNamedWorkspaceRules(arg.workspace))?;
        }
        SubCommand::ClearAllWorkspaceRules => {
            send_message(&SocketMessage::ClearAllWorkspaceRules)?;
        }
        SubCommand::EnforceWorkspaceRules => {
            send_message(&SocketMessage::EnforceWorkspaceRules)?;
        }
        SubCommand::EagerFocus(arg) => {
            send_message(&SocketMessage::EagerFocus(arg.exe))?;
        }
        SubCommand::NewWorkspace => {
            send_message(&SocketMessage::NewWorkspace)?;
        }
        SubCommand::WorkspaceName(name) => {
            send_message(&SocketMessage::WorkspaceName(
                name.monitor,
                name.workspace,
                name.value,
            ))?;
        }
        SubCommand::ResizeDelta(arg) => {
            send_message(&SocketMessage::ResizeDelta(arg.pixels))?;
        }
        SubCommand::MonitorWorkAreaOffset(arg) => {
            send_message(&SocketMessage::MonitorWorkAreaOffset(
                arg.monitor,
                Rect {
                    left: arg.left,
                    top: arg.top,
                    right: arg.right,
                    bottom: arg.bottom,
                },
            ))?;
        }
        SubCommand::GlobalWorkAreaOffset(arg) => {
            send_message(&SocketMessage::WorkAreaOffset(Rect {
                left: arg.left,
                top: arg.top,
                right: arg.right,
                bottom: arg.bottom,
            }))?;
        }

        SubCommand::WorkspaceWorkAreaOffset(arg) => {
            send_message(&SocketMessage::WorkspaceWorkAreaOffset(
                arg.monitor,
                arg.workspace,
                Rect {
                    left: arg.left,
                    top: arg.top,
                    right: arg.right,
                    bottom: arg.bottom,
                },
            ))?;
        }
        SubCommand::ContainerPadding(arg) => {
            send_message(&SocketMessage::ContainerPadding(
                arg.monitor,
                arg.workspace,
                arg.size,
            ))?;
        }
        SubCommand::NamedWorkspaceContainerPadding(arg) => {
            send_message(&SocketMessage::NamedWorkspaceContainerPadding(
                arg.workspace,
                arg.size,
            ))?;
        }
        SubCommand::WorkspacePadding(arg) => {
            send_message(&SocketMessage::WorkspacePadding(
                arg.monitor,
                arg.workspace,
                arg.size,
            ))?;
        }
        SubCommand::NamedWorkspacePadding(arg) => {
            send_message(&SocketMessage::NamedWorkspacePadding(
                arg.workspace,
                arg.size,
            ))?;
        }
        SubCommand::FocusedWorkspacePadding(arg) => {
            send_message(&SocketMessage::FocusedWorkspacePadding(arg.size))?;
        }
        SubCommand::FocusedWorkspaceContainerPadding(arg) => {
            send_message(&SocketMessage::FocusedWorkspaceContainerPadding(arg.size))?;
        }
        SubCommand::AdjustWorkspacePadding(arg) => {
            send_message(&SocketMessage::AdjustWorkspacePadding(
                arg.sizing,
                arg.adjustment,
            ))?;
        }
        SubCommand::AdjustContainerPadding(arg) => {
            send_message(&SocketMessage::AdjustContainerPadding(
                arg.sizing,
                arg.adjustment,
            ))?;
        }
        SubCommand::WorkspaceLayout(arg) => {
            send_message(&SocketMessage::WorkspaceLayout(
                arg.monitor,
                arg.workspace,
                arg.value,
            ))?;
        }
        SubCommand::NamedWorkspaceLayout(arg) => {
            send_message(&SocketMessage::NamedWorkspaceLayout(
                arg.workspace,
                arg.value,
            ))?;
        }

        SubCommand::WorkspaceTiling(arg) => {
            send_message(&SocketMessage::WorkspaceTiling(
                arg.monitor,
                arg.workspace,
                arg.value.into(),
            ))?;
        }
        SubCommand::NamedWorkspaceTiling(arg) => {
            send_message(&SocketMessage::NamedWorkspaceTiling(
                arg.workspace,
                arg.value.into(),
            ))?;
        }
        SubCommand::ScrollingLayoutColumns(arg) => {
            send_message(&SocketMessage::ScrollingLayoutColumns(arg.count))?;
        }
        SubCommand::ClearWorkspaceLayoutRules(arg) => {
            send_message(&SocketMessage::ClearWorkspaceLayoutRules(
                arg.monitor,
                arg.workspace,
            ))?;
        }
        SubCommand::ClearNamedWorkspaceLayoutRules(arg) => {
            send_message(&SocketMessage::ClearNamedWorkspaceLayoutRules(
                arg.workspace,
            ))?;
        }
        SubCommand::EnsureWorkspaces(workspaces) => {
            send_message(&SocketMessage::EnsureWorkspaces(
                workspaces.monitor,
                workspaces.workspace_count,
            ))?;
        }
        SubCommand::EnsureNamedWorkspaces(arg) => {
            send_message(&SocketMessage::EnsureNamedWorkspaces(
                arg.monitor,
                arg.names,
            ))?;
        }
        SubCommand::WorkspaceLayoutRule(arg) => {
            send_message(&SocketMessage::WorkspaceLayoutRule(
                arg.monitor,
                arg.workspace,
                arg.at_container_count,
                arg.layout,
            ))?;
        }
        SubCommand::NamedWorkspaceLayoutRule(arg) => {
            send_message(&SocketMessage::NamedWorkspaceLayoutRule(
                arg.workspace,
                arg.at_container_count,
                arg.layout,
            ))?;
        }
        SubCommand::CrossMonitorMoveBehaviour(arg) => {
            send_message(&SocketMessage::CrossMonitorMoveBehaviour(
                arg.move_behaviour,
            ))?;
        }
        SubCommand::UnmanagedWindowOperationBehaviour(arg) => {
            send_message(&SocketMessage::UnmanagedWindowOperationBehaviour(
                arg.operation_behaviour,
            ))?;
        }
        SubCommand::ToggleMouseFollowsFocus => {
            send_message(&SocketMessage::ToggleMouseFollowsFocus)?;
        }
        SubCommand::MouseFollowsFocus(arg) => {
            send_message(&SocketMessage::MouseFollowsFocus(arg.boolean_state.into()))?;
        }
        SubCommand::ApplicationSpecificConfigurationSchema => {
            #[cfg(feature = "schemars")]
            {
                let asc = schemars::schema_for!(komorebi_client::ApplicationSpecificConfiguration);
                let schema = serde_json::to_string_pretty(&asc)?;
                println!("{schema}");
            }
        }
        SubCommand::NotificationSchema => {
            #[cfg(feature = "schemars")]
            {
                let notification = schemars::schema_for!(komorebi_client::Notification);
                let schema = serde_json::to_string_pretty(&notification)?;
                println!("{schema}");
            }
        }
        SubCommand::SocketSchema => {
            #[cfg(feature = "schemars")]
            {
                let socket_message = schemars::schema_for!(SocketMessage);
                let schema = serde_json::to_string_pretty(&socket_message)?;
                println!("{schema}");
            }
        }
        SubCommand::StaticConfigSchema => {
            #[cfg(feature = "schemars")]
            {
                let settings = schemars::r#gen::SchemaSettings::default().with(|s| {
                    s.option_nullable = false;
                    s.option_add_null_type = false;
                    s.inline_subschemas = true;
                });

                let generator = settings.into_generator();
                let socket_message =
                    generator.into_root_schema_for::<komorebi_client::StaticConfig>();
                let schema = serde_json::to_string_pretty(&socket_message)?;
                println!("{schema}");
            }
        }
        SubCommand::SubscribeSocket(arg) => {
            send_message(&SocketMessage::AddSubscriberSocket(arg.socket))?;
        }
        SubCommand::UnsubscribeSocket(arg) => {
            send_message(&SocketMessage::RemoveSubscriberSocket(arg.socket))?;
        }
        SubCommand::GenerateStaticConfig => {
            print_query(&SocketMessage::GenerateStaticConfig);
        }
        SubCommand::QuickSaveResize => {
            send_message(&SocketMessage::QuickSave)?;
        }
        SubCommand::QuickLoadResize => {
            send_message(&SocketMessage::QuickLoad)?;
        }
        SubCommand::SaveResize(arg) => {
            send_message(&SocketMessage::Save(arg.path))?;
        }
        SubCommand::LoadResize(arg) => {
            send_message(&SocketMessage::Load(arg.path))?;
        }
        SubCommand::DisplayIndexPreference(arg) => {
            send_message(&SocketMessage::DisplayIndexPreference(
                arg.index_preference,
                arg.display,
            ))?;
        }
        SubCommand::ReplaceConfiguration(arg) => {
            send_message(&SocketMessage::ReplaceConfiguration(arg.path))?;
        }
        SubCommand::Border(arg) => {
            send_message(&SocketMessage::Border(arg.boolean_state.into()))?;
        }
        SubCommand::BorderColour(arg) => {
            send_message(&SocketMessage::BorderColour(
                arg.window_kind,
                arg.r,
                arg.g,
                arg.b,
            ))?;
        }
        SubCommand::BorderWidth(arg) => {
            send_message(&SocketMessage::BorderWidth(arg.width))?;
        }
        SubCommand::BorderOffset(arg) => {
            send_message(&SocketMessage::BorderOffset(arg.offset))?;
        }
    }

    Ok(())
}

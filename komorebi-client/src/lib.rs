pub use komorebi::DATA_DIR;
pub use komorebi::Notification;
pub use komorebi::NotificationEvent;
pub use komorebi::animation::AnimationPrefix;
pub use komorebi::container::Container;
pub use komorebi::core::ApplicationIdentifier;
pub use komorebi::core::MoveBehaviour;
pub use komorebi::core::OperationBehaviour;
pub use komorebi::core::Sizing;
pub use komorebi::core::SocketMessage;
pub use komorebi::core::StateQuery;
pub use komorebi::core::SubscribeOptions;
pub use komorebi::core::WindowKind;
pub use komorebi::core::animation::AnimationStyle;
pub use komorebi::core::arrangement::Axis;
pub use komorebi::core::asc::ApplicationSpecificConfiguration;
pub use komorebi::core::cycle_direction::CycleDirection;
pub use komorebi::core::default_layout::DefaultLayout;
pub use komorebi::core::layout::Layout;
pub use komorebi::core::operation_direction::OperationDirection;
pub use komorebi::core::pathext::PathExt;
pub use komorebi::core::pathext::replace_env_in_path;
pub use komorebi::core::rect::Rect;
pub use komorebi::monitor_reconciliator::MonitorNotification;
pub use komorebi::splash;
pub use komorebi::state::State;
pub use komorebi::static_config::StaticConfig;
pub use komorebi::window::Window;
pub use komorebi::workspace::Workspace;
pub use komorebi::workspace::WorkspaceLayer;
pub use komorebi_themes::KomorebiTheme;
pub use komorebi_themes::colour::Colour;
pub use komorebi_themes::colour::Rgb;
use std::borrow::Borrow;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::time::Duration;

const KOMOREBI: &str = "komorebi.sock";

pub fn send_message(message: &SocketMessage) -> std::io::Result<()> {
    let socket = DATA_DIR.join(KOMOREBI);
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(serde_json::to_string(message)?.as_bytes())
}

pub fn send_batch<Q>(messages: impl IntoIterator<Item = Q>) -> std::io::Result<()>
where
    Q: Borrow<SocketMessage>,
{
    let socket = DATA_DIR.join(KOMOREBI);
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let msgs = messages.into_iter().fold(String::new(), |mut s, m| {
        if let Ok(m_str) = serde_json::to_string(m.borrow()) {
            s.push_str(&m_str);
            s.push('\n');
        }
        s
    });
    stream.write_all(msgs.as_bytes())
}

pub fn send_query(message: &SocketMessage) -> std::io::Result<String> {
    let socket = DATA_DIR.join(KOMOREBI);

    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(serde_json::to_string(message)?.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_to_string(&mut response)?;

    Ok(response)
}

pub fn subscribe(name: &str) -> std::io::Result<UnixListener> {
    let socket = DATA_DIR.join(name);

    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(error) => match error.kind() {
            std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(error);
            }
        },
    };

    let listener = UnixListener::bind(&socket)?;

    send_message(&SocketMessage::AddSubscriberSocket(name.to_string()))?;

    Ok(listener)
}

pub fn subscribe_with_options(
    name: &str,
    options: SubscribeOptions,
) -> std::io::Result<UnixListener> {
    let socket = DATA_DIR.join(name);

    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(error) => match error.kind() {
            std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(error);
            }
        },
    };

    let listener = UnixListener::bind(&socket)?;

    send_message(&SocketMessage::AddSubscriberSocketWithOptions(
        name.to_string(),
        options,
    ))?;

    Ok(listener)
}

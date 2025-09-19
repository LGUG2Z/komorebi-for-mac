pub use komorebi::DATA_DIR;
pub use komorebi::core::Sizing;
pub use komorebi::core::SocketMessage;
pub use komorebi::core::arrangement::Axis;
pub use komorebi::core::cycle_direction::CycleDirection;
pub use komorebi::core::default_layout::DefaultLayout;
pub use komorebi::core::operation_direction::OperationDirection;
pub use komorebi::core::pathext::PathExt;
use std::borrow::Borrow;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
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

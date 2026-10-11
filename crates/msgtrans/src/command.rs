use crate::SessionId;

/// What is actually known about one connection.
///
/// Every field here is real and set by the adapter that owns the socket. 2.0
/// deleted the `last_activity`/`packets_sent`/`packets_received`/`bytes_sent`/
/// `bytes_received` fields: no adapter ever updated them, so a handler reading
/// them got a constant zero that looked like a live counter. If per-connection
/// traffic accounting is needed, it belongs in a metrics facility that is
/// actually wired, not in a struct that silently reports zero.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ConnectionInfo {
    /// Session ID
    pub session_id: SessionId,
    /// Local address
    pub local_addr: std::net::SocketAddr,
    /// Remote address
    pub peer_addr: std::net::SocketAddr,
    /// Protocol type
    pub protocol: String,
    /// Connection state
    pub state: ConnectionState,
    /// Established time
    pub established_at: std::time::SystemTime,
    /// Closed time
    pub closed_at: Option<std::time::SystemTime>,
}

impl Default for ConnectionInfo {
    fn default() -> Self {
        let now = std::time::SystemTime::now();
        Self {
            session_id: SessionId::new(0),
            local_addr: "0.0.0.0:0".parse().unwrap(),
            peer_addr: "0.0.0.0:0".parse().unwrap(),
            protocol: "tcp".to_string(),
            state: ConnectionState::Connecting,
            established_at: now,
            closed_at: None,
        }
    }
}

impl ConnectionInfo {
    /// How long this connection has been up.
    pub fn connection_duration(&self) -> std::time::Duration {
        self.established_at.elapsed().unwrap_or_default()
    }
}

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionState {
    /// Connecting
    Connecting,
    /// Connected
    Connected,
    /// Closing
    Closing,
    /// Closed
    Closed,
    /// Paused
    Paused,
    /// Error state
    Error,
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::Connecting => write!(f, "Connecting"),
            ConnectionState::Connected => write!(f, "Connected"),
            ConnectionState::Closing => write!(f, "Closing"),
            ConnectionState::Closed => write!(f, "Closed"),
            ConnectionState::Paused => write!(f, "Paused"),
            ConnectionState::Error => write!(f, "Error"),
        }
    }
}

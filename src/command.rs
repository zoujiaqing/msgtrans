use crate::SessionId;

/// Connection information
#[derive(Debug, Clone)]
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
    /// Last activity time
    pub last_activity: std::time::SystemTime,
    /// Packets sent count
    pub packets_sent: u64,
    /// Packets received count
    pub packets_received: u64,
    /// Bytes sent count
    pub bytes_sent: u64,
    /// Bytes received count
    pub bytes_received: u64,
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
            last_activity: now,
            packets_sent: 0,
            packets_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
        }
    }
}

impl ConnectionInfo {
    pub fn update_activity(&mut self) {
        self.last_activity = std::time::SystemTime::now();
    }

    pub fn record_packet_sent(&mut self, size: usize) {
        self.packets_sent += 1;
        self.bytes_sent += size as u64;
        self.update_activity();
    }

    pub fn record_packet_received(&mut self, size: usize) {
        self.packets_received += 1;
        self.bytes_received += size as u64;
        self.update_activity();
    }

    pub fn connection_duration(&self) -> std::time::Duration {
        self.established_at.elapsed().unwrap_or_default()
    }

    pub fn idle_duration(&self) -> std::time::Duration {
        self.last_activity.elapsed().unwrap_or_default()
    }
}

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

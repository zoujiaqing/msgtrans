use crate::{error::TransportError, packet::Packet, SessionId};
use tokio::sync::oneshot;

/// Unified abstraction for transport layer commands
#[derive(Debug)]
pub enum TransportCommand {
    /// Send packet
    Send {
        session_id: SessionId,
        packet: Packet,
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },

    /// Close connection
    Close {
        session_id: SessionId,
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },

    /// Configuration update
    Configure {
        config: ConfigUpdate,
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },

    /// Get statistics information
    GetStats {
        response_tx: oneshot::Sender<TransportStats>,
    },

    /// Get connection information
    GetConnectionInfo {
        session_id: SessionId,
        response_tx: oneshot::Sender<Result<ConnectionInfo, TransportError>>,
    },

    /// Get all active sessions
    GetActiveSessions {
        response_tx: oneshot::Sender<Vec<SessionId>>,
    },

    /// Force disconnect session
    ForceDisconnect {
        session_id: SessionId,
        reason: String,
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },

    /// Pause session
    PauseSession {
        session_id: SessionId,
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },

    /// Resume session
    ResumeSession {
        session_id: SessionId,
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },
}

/// Configuration update types
#[derive(Debug)]
pub enum ConfigUpdate {
    /// Set buffer size
    BufferSize(usize),
    /// Set timeout duration
    Timeout(std::time::Duration),
    /// Set maximum connections
    MaxConnections(usize),
    /// Protocol-specific configuration
    Protocol(Box<dyn std::any::Any + Send>),
}

/// Transport statistics information
#[derive(Debug, Clone)]
pub struct TransportStats {
    /// Total packets sent
    pub packets_sent: u64,
    /// Total packets received
    pub packets_received: u64,
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Active connections
    pub active_connections: u64,
    /// Total connections (historical)
    pub total_connections: u64,
    /// Error count
    pub errors: u64,
    /// Start time
    pub start_time: std::time::SystemTime,
    /// Last activity time
    pub last_activity: std::time::SystemTime,
}

impl Default for TransportStats {
    fn default() -> Self {
        let now = std::time::SystemTime::now();
        Self {
            packets_sent: 0,
            packets_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
            active_connections: 0,
            total_connections: 0,
            errors: 0,
            start_time: now,
            last_activity: now,
        }
    }
}

impl TransportStats {
    pub fn new() -> Self {
        Default::default()
    }

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

    pub fn record_connection_opened(&mut self) {
        self.active_connections += 1;
        self.total_connections += 1;
        self.update_activity();
    }

    pub fn record_connection_closed(&mut self) {
        if self.active_connections > 0 {
            self.active_connections -= 1;
        }
        self.update_activity();
    }

    pub fn record_error(&mut self) {
        self.errors += 1;
        self.update_activity();
    }

    pub fn uptime(&self) -> std::time::Duration {
        self.start_time.elapsed().unwrap_or_default()
    }

    pub fn idle_time(&self) -> std::time::Duration {
        self.last_activity.elapsed().unwrap_or_default()
    }
}

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

/// Protocol-specific command trait
///
/// This trait allows each protocol to define its own specific command types
pub trait ProtocolCommand: Send + std::fmt::Debug + 'static {
    type Response: Send;

    /// Convert to generic transport command
    fn into_transport_command(self) -> TransportCommand;

    /// Execute command
    fn execute(self) -> impl std::future::Future<Output = Self::Response> + Send;
}

/// Command builder
pub struct CommandBuilder;

impl CommandBuilder {
    /// Create send command
    pub fn send(
        session_id: SessionId,
        packet: Packet,
    ) -> (
        TransportCommand,
        oneshot::Receiver<Result<(), TransportError>>,
    ) {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TransportCommand::Send {
            session_id,
            packet,
            response_tx,
        };
        (command, response_rx)
    }

    /// Create close command
    pub fn close(
        session_id: SessionId,
    ) -> (
        TransportCommand,
        oneshot::Receiver<Result<(), TransportError>>,
    ) {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TransportCommand::Close {
            session_id,
            response_tx,
        };
        (command, response_rx)
    }

    /// Create get statistics command
    pub fn get_stats() -> (TransportCommand, oneshot::Receiver<TransportStats>) {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TransportCommand::GetStats { response_tx };
        (command, response_rx)
    }

    /// Create get connection info command
    pub fn get_connection_info(
        session_id: SessionId,
    ) -> (
        TransportCommand,
        oneshot::Receiver<Result<ConnectionInfo, TransportError>>,
    ) {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TransportCommand::GetConnectionInfo {
            session_id,
            response_tx,
        };
        (command, response_rx)
    }

    /// Create get active sessions command
    pub fn get_active_sessions() -> (TransportCommand, oneshot::Receiver<Vec<SessionId>>) {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TransportCommand::GetActiveSessions { response_tx };
        (command, response_rx)
    }

    /// Create force disconnect command
    pub fn force_disconnect(
        session_id: SessionId,
        reason: String,
    ) -> (
        TransportCommand,
        oneshot::Receiver<Result<(), TransportError>>,
    ) {
        let (response_tx, response_rx) = oneshot::channel();
        let command = TransportCommand::ForceDisconnect {
            session_id,
            reason,
            response_tx,
        };
        (command, response_rx)
    }
}

/// Command executor
pub struct CommandExecutor;

impl CommandExecutor {
    /// Execute send command and wait for result
    pub async fn send_and_wait(
        command_tx: &tokio::sync::mpsc::Sender<TransportCommand>,
        session_id: SessionId,
        packet: Packet,
    ) -> Result<(), TransportError> {
        let (command, response_rx) = CommandBuilder::send(session_id, packet);

        command_tx
            .send(command)
            .await
            .map_err(|_| TransportError::connection_error("Channel closed", false))?;

        response_rx
            .await
            .map_err(|_| TransportError::connection_error("Channel closed", false))?
    }

    /// Close connection and wait for result
    pub async fn close_and_wait(
        command_tx: &tokio::sync::mpsc::Sender<TransportCommand>,
        session_id: SessionId,
    ) -> Result<(), TransportError> {
        let (command, response_rx) = CommandBuilder::close(session_id);

        command_tx
            .send(command)
            .await
            .map_err(|_| TransportError::connection_error("Channel closed", false))?;

        response_rx
            .await
            .map_err(|_| TransportError::connection_error("Channel closed", false))?
    }

    /// Get statistics information
    pub async fn get_stats(
        command_tx: &tokio::sync::mpsc::Sender<TransportCommand>,
    ) -> Result<TransportStats, TransportError> {
        let (command, response_rx) = CommandBuilder::get_stats();

        command_tx
            .send(command)
            .await
            .map_err(|_| TransportError::connection_error("Channel closed", false))?;

        response_rx
            .await
            .map_err(|_| TransportError::connection_error("Channel closed", false))
    }
}

// The README is the crate documentation, which also makes its examples part of
// the doctest suite so they cannot drift from the real API.
#![doc = include_str!("../README.md")]
#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(dead_code)]
#![allow(private_bounds)]
#![allow(private_interfaces)]
#![allow(async_fn_in_trait)]
#![allow(unused_must_use)]
#![allow(non_upper_case_globals)]

/// Transport layer: client, server, session actors and request lifecycle.
pub mod transport;

// Protocol adapters
pub mod adapters;

// Protocol abstraction
pub mod protocol;

// Core types
pub mod command;
pub mod error;
pub mod event;
pub mod packet;
pub mod stream;

// New modules
pub mod connection;
#[deprecated(
    since = "1.0.9",
    note = "experimental plugin system, not wired into the transport path; scheduled for removal in 2.0"
)]
pub mod plugin;

// Type definitions
pub type PacketId = u32;

/// Type-safe wrapper for session ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(u64);

impl SessionId {
    /// Create new session ID
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get raw ID value
    pub fn as_u64(&self) -> u64 {
        self.0
    }

    /// Generate next session ID
    pub fn next(&self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session-{}", self.0)
    }
}

impl From<u64> for SessionId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<SessionId> for u64 {
    fn from(session_id: SessionId) -> Self {
        session_id.0
    }
}

// Re-export core types
pub use command::{ConnectionInfo, TransportCommand, TransportStats};
pub use error::{CloseReason, TransportError};
pub use event::{ClientEvent, QuicEvent, TcpEvent, TransportEvent, WebSocketEvent};
pub use packet::{FramePolicy, Packet, PacketError, PacketType};
pub use stream::{ClientEventStream, EventStream, PacketStream};

// ConnectionPool / ExpertConfig below are deprecated; re-exporting them here is
// intentional for backward compatibility until 2.0.
#[allow(deprecated)]
pub use transport::{
    AcceptorConfig, BackpressureStrategy, CircuitBreakerConfig, ConnectionPool,
    ConnectionPoolConfig, ExpertConfig, LoadBalancerConfig, LockFreeCounter, LockFreeHashMap,
    LockFreeQueue, MemoryPool, MemoryStats, MemoryStatsSnapshot, PerformanceConfig, ProtocolStats,
    RateLimiterConfig, RetryConfig, SmartPoolConfig, Transport, TransportClient,
    TransportClientBuilder, TransportConfig, TransportContext, TransportServer,
    TransportServerBuilder,
};

pub use protocol::{
    ClientConfig, QuicClientConfig, QuicServerConfig, ServerConfig, TcpClientConfig,
    TcpServerConfig, WebSocketClientConfig, WebSocketServerConfig,
};
// Re-export new abstractions
pub use connection::{Connection, ConnectionFactory, Server};
#[allow(deprecated)]
#[deprecated(
    since = "1.0.9",
    note = "experimental plugin system, not wired into the transport path; scheduled for removal in 2.0"
)]
pub use plugin::{PluginInfo, PluginManager, ProtocolPlugin};

// Re-export tokio for user convenience
pub use tokio;

// Convenient type aliases
pub type Result<T> = std::result::Result<T, TransportError>;

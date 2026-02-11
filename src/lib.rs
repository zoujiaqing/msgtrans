#![allow(unused_variables)]
#![allow(unused_mut)]
#![allow(dead_code)]
#![allow(private_bounds)]
#![allow(private_interfaces)]
#![allow(async_fn_in_trait)]
#![allow(unused_must_use)]
#![allow(non_upper_case_globals)]

/// msgtrans - Unified multi-protocol transport library
///
/// This is a modern, high-performance Rust transport library providing unified interfaces for TCP, WebSocket and QUIC protocols.
/// Designed based on Actor pattern, completely eliminates callback hell and provides type-safe event-driven API.
// Transport layer
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
pub use packet::{Packet, PacketError, PacketType};
pub use stream::{ClientEventStream, EventStream, PacketStream};

pub use transport::{
    AcceptorConfig,
    // High-performance components
    Actor,
    ActorManager,
    BackpressureStrategy,
    CircuitBreakerConfig,
    // Connection pool and memory management
    ConnectionPool,
    // Advanced configuration
    ConnectionPoolConfig,
    ExpertConfig,
    LoadBalancerConfig,
    LockFreeCounter,
    // LockFree base components
    LockFreeHashMap,
    LockFreeQueue,
    MemoryPool,
    MemoryStats,
    MemoryStatsSnapshot,
    PerformanceConfig,
    ProtocolAdapter,
    ProtocolStats,
    RateLimiterConfig,
    RetryConfig,
    SmartPoolConfig,
    // Core transport types
    Transport,
    // Client and server
    TransportClient,
    // Builders
    TransportClientBuilder,
    TransportConfig,
    TransportServer,
    TransportServerBuilder,
};

pub use protocol::{
    ClientConfig, QuicClientConfig, QuicServerConfig, ServerConfig, TcpClientConfig,
    TcpServerConfig, WebSocketClientConfig, WebSocketServerConfig,
};
// Re-export new abstractions
pub use connection::{Connection, ConnectionFactory, Server};
pub use plugin::{PluginInfo, PluginManager, ProtocolPlugin};

// Re-export tokio for user convenience
pub use tokio;

// Convenient type aliases
pub type Result<T> = std::result::Result<T, TransportError>;

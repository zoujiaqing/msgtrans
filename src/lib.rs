// The README is the crate documentation, which also makes its examples part of
// the doctest suite so they cannot drift from the real API. It documents the
// default (all-protocols) build and its examples import TCP/WebSocket/QUIC
// types freely, so it is only attached when all three features are enabled —
// partial builds skip the README doctests instead of failing to compile them.
#![cfg_attr(
    all(feature = "tcp", feature = "websocket", feature = "quic"),
    doc = include_str!("../README.md")
)]
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
#[cfg(feature = "quic")]
pub use event::QuicEvent;
#[cfg(feature = "tcp")]
pub use event::TcpEvent;
#[cfg(feature = "websocket")]
pub use event::WebSocketEvent;
pub use event::{ClientEvent, RespondOutcome, TransportEvent};
pub use packet::{FramePolicy, Packet, PacketError, PacketType};
pub use stream::{ClientEvents, EventStream, PacketStream};

pub use transport::{
    LockFreeCounter, LockFreeHashMap, LockFreeQueue, MemoryPool, MemoryStats, MemoryStatsSnapshot,
    ProtocolStats, RetryConfig, Transport, TransportClient, TransportClientBuilder,
    TransportConfig, TransportContext, TransportServer, TransportServerBuilder,
};

pub use protocol::{ClientConfig, ServerConfig};
#[cfg(feature = "quic")]
pub use protocol::{QuicClientConfig, QuicServerConfig};
#[cfg(feature = "tcp")]
pub use protocol::{TcpClientConfig, TcpServerConfig};
#[cfg(feature = "websocket")]
pub use protocol::{WebSocketClientConfig, WebSocketServerConfig};
// Re-export new abstractions
pub use connection::{Connection, ConnectionFactory, Server};

// Convenient type aliases
pub type Result<T> = std::result::Result<T, TransportError>;

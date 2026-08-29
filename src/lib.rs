// The README is the crate documentation, which also makes its examples part of
// the doctest suite so they cannot drift from the real API. It documents the
// default (all-protocols) build and its examples import TCP/WebSocket/QUIC
// types freely, so it is only attached when all three features are enabled —
// partial builds skip the README doctests instead of failing to compile them.
#![cfg_attr(
    all(feature = "tcp", feature = "websocket", feature = "quic"),
    doc = include_str!("../README.md")
)]
// Idiomatic builder patterns (`let mut x = Default::default(); x.field = ..`)
// and the transport::transport module name are intentional.
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::module_inception)]
// Adapter infrastructure (core/events/outbound) is only exercised when at
// least one protocol adapter is compiled in; a no-protocol build is a
// degenerate config nobody ships. Allow its dead code ONLY there — every
// real (>=1 protocol) build keeps -D warnings strict.
#![cfg_attr(
    not(any(feature = "tcp", feature = "websocket", feature = "quic")),
    allow(dead_code)
)]
#![allow(async_fn_in_trait)]
#![allow(non_upper_case_globals)]

// Implementation modules are crate-private: the public API is exactly the
// crate root re-exports below plus `msgtrans::spi`. Reaching internal types
// through deep paths (`msgtrans::transport::Transport`, `msgtrans::event::*`,
// …) does not resolve.
/// The Chinese README, attached as hidden docs purely so ITS examples are
/// compiled as doctests too.
///
/// The English README was gated this way from the start; the Chinese one was
/// not, so it silently kept 1.x APIs (private deep paths, `packet.payload`
/// field access, `TransportResult.data`) while every other gate passed. Same
/// feature condition as the English README, and `doc(hidden)` so it does not
/// duplicate the rendered crate docs.
#[cfg(all(feature = "tcp", feature = "websocket", feature = "quic"))]
#[doc(hidden)]
#[doc = include_str!("../README.zh-CN.md")]
pub mod readme_zh_cn_doctests {}

pub(crate) mod adapters;

/// Validate a server TLS certificate/key pair at startup (parse + key/cert match).
/// Exposed so hosts can fail fast on bad TLS material before binding a listener.
#[cfg(feature = "quic")]
pub use adapters::quic::validate_server_tls_material;
pub(crate) mod command;
pub(crate) mod connection;
pub(crate) mod error;
pub(crate) mod event;
pub(crate) mod packet;
pub(crate) mod protocol;
pub(crate) mod stream;
pub(crate) mod transport;

/// Stable extension SPI for implementing a custom transport protocol.
pub mod spi;

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
pub use command::{ConnectionInfo, ConnectionState};
pub use error::{CloseReason, TransportError};
pub use event::{
    ClientEvent, ClientMessage, ClientRequest, RespondOutcome, SendReceipt, TransportEvent,
};
pub use packet::{
    CompressionType, DecodeLimits, FramePolicy, Packet, PacketError, PacketType, ReservedFlags,
    DEFAULT_MAX_FRAME_SIZE,
};
pub use stream::ClientEvents;

pub use transport::{
    BroadcastReport, RequestOptions, Responder, RetryConfig, SendOptions, SessionHandler,
    SessionSender, ShutdownReport, TransportClient, TransportClientBuilder, TransportConfig,
    TransportServer, TransportServerBuilder,
};

#[cfg(feature = "websocket")]
pub use protocol::{
    ClientTls, WebSocketClientConfig, WebSocketServerConfig, WS_SUBPROTOCOL_MSGTRANS,
};
#[cfg(feature = "quic")]
pub use protocol::{QuicClientConfig, QuicServerConfig};
#[cfg(feature = "tcp")]
pub use protocol::{TcpClientConfig, TcpServerConfig};
// The extension SPI (Connection / ConnectionWriter / Server / WriteCompletion
// and the Dyn* config traits) is reachable ONLY through `msgtrans::spi`.
// Exporting it from the crate root too gave every type two public paths and
// blurred the line between "API for applications" and "SPI for protocol
// implementors".
// The tuning defaults stay CRATE-INTERNAL: `Default` plus the getters already
// show a caller the values in effect, so re-exporting the constants would
// widen the frozen surface for nothing.
pub use transport::limits::{ClientLimits, ConnectionLimits, ServerLimits};

// Convenient type aliases
pub type Result<T> = std::result::Result<T, TransportError>;

pub(crate) mod client;
/// TRANSPORT High-performance transport layer module
///
/// Provides unified transport abstraction with support for multiple protocols,
/// lock-free data structures, and intelligent runtime optimization.
///
/// ## Architecture Overview
/// - **Backend Performance**: Lock-free connection pools and optimized memory management
/// - **Frontend Compatibility**: Unified API with backward compatibility
/// - **Progressive Enhancement**: Smart components ready for seamless integration
///
/// ## High-Performance Component Architecture
/// ```text
/// ┌─────────────────────────────────────────────────────────────┐
/// │                   High-Performance Transport                │
/// ├─────────────────┬───────────────────┬───────────────────────┤
/// │   Frontend API   │    Core Processing │    Backend Storage    │
/// ├─────────────────┼───────────────────┼───────────────────────┤
/// │ Transport API    │ Generic Actor     │ Concurrent Pools        │
/// │ Unified Interface│ Legacy Compatible │ Optimized Memory      │
/// │ Zero Config      │ Ready for Upgrade │ Detailed Monitoring   │
/// └─────────────────┴───────────────────┴───────────────────────┘
/// ```
///
/// ## User Experience
/// - **Zero Configuration**: `TransportClientBuilder::new().build()` and
///   `TransportServerBuilder::new().build()` automatically enable optimizations
/// - **Full Backward Compatibility**: Existing code works without modification
/// - **Transparent Performance**: Memory allocation and connection management
///   automatically use high-performance implementations
pub(crate) mod config;
pub(crate) mod connection_state;
pub(crate) mod limits;
pub(crate) mod request_registry;
pub(crate) mod server;
pub(crate) mod transport;
pub(crate) mod transport_server;

pub(crate) mod concurrent;
pub(crate) mod context;
// The read-buffer pool is a byte-stream (TCP/QUIC) optimization; WebSocket
// framing does its own buffering, so the pool is gated to those protocols.
#[cfg(any(feature = "tcp", feature = "quic"))]
pub(crate) mod memory_pool;
pub(crate) mod session_actor;

// [EXPORTS] Re-export core APIs with unified architecture
pub use client::{RetryConfig, TransportClient, TransportClientBuilder};
pub use server::TransportServerBuilder;
pub use transport_server::{BroadcastReport, ShutdownReport, TransportServer};
// Extension SPI (used by custom protocol adapters) reachable via the crate root.

// [CONFIG] Configuration exports
pub use config::TransportConfig;

// Internal performance machinery (memory pool, lock-free primitives) is not
// part of the public API; the modules stay crate-internal.

// [CONNECTION] Lock-free connection exports

// [STATE] Connection state management exports
// Request lifecycle state machine is internal (crate::transport::request_registry).

use crate::packet::CompressionType;
use bytes::Bytes;
use std::time::Duration;

/// What a caller may shape about ONE outgoing message.
///
/// Deliberately does NOT contain a message id or packet type: those belong to
/// the transport (see `TransportServer::send_with_options`), which is what makes
/// a caller-numbered `Request` unrepresentable.
#[derive(Default, Clone, Debug)]
pub struct SendOptions {
    /// Application-layer business type ID.
    pub(crate) biz_type: Option<u8>,
    /// Extended header content (business layer encoded).
    pub(crate) ext_header: Option<Bytes>,
    /// Compress the payload before sending.
    pub(crate) compression: Option<CompressionType>,
}

impl SendOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the business type.
    pub fn biz_type(mut self, biz_type: u8) -> Self {
        self.biz_type = Some(biz_type);
        self
    }

    /// Set the extension header.
    pub fn ext_header(mut self, ext_header: Bytes) -> Self {
        self.ext_header = Some(ext_header);
        self
    }

    /// Compress the payload with `compression` before it goes on the wire.
    ///
    /// The transport performs the compression, so the header and body can never
    /// disagree: if the codec feature is not compiled in (or compression
    /// fails), the send returns an error instead of shipping a raw payload
    /// under a "compressed" header — which is exactly what the removed
    /// `TransportOptions::compression` used to do.
    pub fn compression(mut self, compression: CompressionType) -> Self {
        self.compression = Some(compression);
        self
    }
}

/// [`SendOptions`] plus the response deadline that only a request has.
#[derive(Default, Clone, Debug)]
pub struct RequestOptions {
    pub(crate) send: SendOptions,
    /// How long to wait for the response before failing with a timeout.
    pub(crate) timeout: Option<Duration>,
}

impl RequestOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the business type.
    pub fn biz_type(mut self, biz_type: u8) -> Self {
        self.send = self.send.biz_type(biz_type);
        self
    }

    /// Set the extension header.
    pub fn ext_header(mut self, ext_header: Bytes) -> Self {
        self.send = self.send.ext_header(ext_header);
        self
    }

    /// Compress the request payload (see [`SendOptions::compression`]).
    pub fn compression(mut self, compression: CompressionType) -> Self {
        self.send = self.send.compression(compression);
        self
    }

    /// Set the response deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

// [ACTOR] Session actor model exports
// Public handler API: Responder / SessionHandler / SessionSender.
pub use session_actor::{Responder, SessionHandler, SessionSender};
// Actor internals stay crate-private (crate::transport::session_actor).

impl SendOptions {
    /// Stamp these options onto a packet the TRANSPORT built, compressing the
    /// payload if asked.
    ///
    /// Compression happens here, after the body is in place, and a failure is
    /// returned rather than logged: the alternative (what 1.x/alpha.3 did) was
    /// to set the compressed flag first and then ship the raw payload when the
    /// codec was missing, producing a frame no peer could decode.
    pub(crate) fn apply(
        &self,
        packet: &mut crate::packet::Packet,
    ) -> Result<(), crate::error::TransportError> {
        packet.set_biz_type(self.biz_type.unwrap_or(0));
        if let Some(ext) = self.ext_header.as_ref() {
            packet.set_ext_header(ext.clone());
        }
        match self.compression {
            None | Some(CompressionType::None) => Ok(()),
            Some(compression) => {
                packet.set_compression(compression);
                packet.compress_payload().map_err(|e| {
                    crate::error::TransportError::config_error(
                        "compression",
                        format!("failed to compress payload with {compression:?}: {e}"),
                    )
                })
            }
        }
    }
}

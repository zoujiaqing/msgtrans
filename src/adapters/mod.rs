pub(crate) mod core;
#[doc(hidden)]
pub mod events;
pub mod factories;
#[doc(hidden)]
pub mod outbound;
#[cfg(feature = "quic")]
pub mod quic;
/// Protocol adapter implementation module
///
/// This module contains specific adapter implementations for various transport protocols
#[cfg(feature = "tcp")]
pub mod tcp;
#[cfg(feature = "websocket")]
pub mod websocket;

#[cfg(feature = "quic")]
pub use quic::{QuicAdapter, QuicError};
#[cfg(feature = "tcp")]
pub use tcp::{TcpAdapter, TcpError};
#[cfg(feature = "websocket")]
pub use websocket::{WebSocketAdapter, WebSocketError};

pub use factories::create_standard_registry;
#[cfg(feature = "quic")]
pub use factories::{QuicFactory, QuicServerWrapper};
#[cfg(feature = "tcp")]
pub use factories::{TcpFactory, TcpServerWrapper};
#[cfg(feature = "websocket")]
pub use factories::{WebSocketFactory, WebSocketServerWrapper};

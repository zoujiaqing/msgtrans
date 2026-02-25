pub mod factories;
pub mod quic;
/// Protocol adapter implementation module
///
/// This module contains specific adapter implementations for various transport protocols
pub mod tcp;
pub mod websocket;

pub use quic::{QuicAdapter, QuicError};
pub use tcp::{TcpAdapter, TcpError};
pub use websocket::{WebSocketAdapter, WebSocketError};

pub use factories::{create_standard_registry, TcpFactory, TcpServerWrapper};
pub use factories::{QuicFactory, QuicServerWrapper};
pub use factories::{WebSocketFactory, WebSocketServerWrapper};

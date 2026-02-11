pub mod factories;
pub mod quic;
/// Protocol adapter implementation module
///
/// This module contains specific adapter implementations for various transport protocols
pub mod tcp;
pub mod websocket;

// Only export standard APIs that users really need (remove Builder classes)
pub use quic::{QuicAdapter, QuicError};
pub use tcp::{TcpAdapter, TcpError};
pub use websocket::{WebSocketAdapter, WebSocketError};

// Export factory and connection types
pub use factories::{create_standard_registry, TcpFactory, TcpServerWrapper};
pub use factories::{QuicConnection, QuicFactory, QuicServerWrapper};
pub use factories::{WebSocketConnection, WebSocketFactory, WebSocketServerWrapper};

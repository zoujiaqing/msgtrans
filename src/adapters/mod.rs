pub(crate) mod core;
pub(crate) mod events;
pub(crate) mod factories;
pub(crate) mod outbound;
#[cfg(any(feature = "tcp-tls", feature = "quic"))]
pub(crate) mod tls_common;

#[cfg(feature = "quic")]
pub(crate) mod quic;
/// Protocol adapter implementation module
///
/// This module contains specific adapter implementations for various transport protocols
#[cfg(feature = "tcp")]
pub(crate) mod tcp;
#[cfg(feature = "websocket")]
pub(crate) mod websocket;

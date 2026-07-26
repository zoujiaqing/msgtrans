//! Stable extension SPI for implementing a custom transport protocol.
//!
//! Everything an out-of-crate adapter needs — and nothing that leaks an
//! internal type. Implement `Connection` (and `Server` for accept/connect),
//! hand out the write half via `Connection::writer`, drive the read loop by
//! pushing into an `EventSink`, and return the paired `ConnectionEvents` from
//! `Connection::take_event_pipe`. To advertise the config to the builder,
//! implement the object-safe `DynProtocolConfig` plus `DynServerConfig` and/or
//! `DynClientConfig` — one trait set, no generic variant, so no internal
//! adapter type is written into the frozen contract.
//!
//! These types are reachable ONLY here: the crate root is the application API,
//! this module is the protocol-implementor API.

pub use crate::connection::{Connection, ConnectionWriter, Server, WriteCompletion};
pub use crate::packet::Packet;
pub use crate::protocol::adapter::{
    ConfigError, DynClientConfig, DynProtocolConfig, DynServerConfig,
};
pub use crate::transport::limits::ConnectionLimits;
pub use crate::{CloseReason, ConnectionInfo, SessionId, TransportError, TransportEvent};

/// Producer half of a connection's event channel, held by the adapter's read
/// loop.
///
/// The methods are **typed by plane**, not by event: each kind of thing an
/// adapter can report has exactly one method, and that method picks the correct
/// plane. An untyped `deliver(TransportEvent)`/`diagnostic(TransportEvent)`
/// pair let a custom adapter push real data onto the droppable diagnostic
/// channel (silently losing messages under load) or push a close onto the
/// backpressured data queue (losing the close to a full queue) — mistakes the
/// type system now prevents.
///
/// - Data plane ([`Self::message`]): bounded and backpressured, never dropped.
/// - Diagnostic plane ([`Self::message_sent`]): droppable under load, so a
///   backlog of confirmations can never displace real data.
/// - Control plane ([`Self::close`]): never blocks, never queues behind data.
#[derive(Debug)]
pub struct EventSink(pub(crate) crate::adapters::events::EventPipe);

impl EventSink {
    /// Deliver a received packet on the data plane, with backpressure. Returns
    /// `false` when the consumer is gone — the adapter should stop reading.
    pub async fn message(&self, packet: Packet) -> bool {
        self.0
            .deliver(TransportEvent::MessageReceived(packet))
            .await
    }

    /// Report a non-fatal transport error on the data plane (it reaches the
    /// consumer as `ClientEvent::Error`). A fatal condition should use
    /// [`Self::close`] with `CloseReason::Error` instead.
    pub async fn error(&self, error: TransportError) -> bool {
        self.0
            .deliver(TransportEvent::TransportError { error })
            .await
    }

    /// Confirm that a message was written. **Droppable**: this rides the
    /// diagnostic plane and is discarded under load, so it must never be used
    /// as a delivery ledger.
    pub fn message_sent(&self, message_id: crate::PacketId) {
        self.0.diagnostic(TransportEvent::MessageSent {
            packet_id: message_id,
        })
    }

    /// Publish the terminal close reason. Non-blocking and immune to
    /// data-queue pressure; the consumer sees queued data first, then exactly
    /// one `ConnectionClosed`.
    pub fn close(&self, reason: CloseReason) {
        self.0.close(reason)
    }
}

/// Consumer half of a connection's event channel. The transport takes this
/// from the connection exactly once (via [`Connection::take_event_pipe`]) and
/// drains it: every queued data event, then exactly one `ConnectionClosed`.
#[derive(Debug)]
pub struct ConnectionEvents(pub(crate) crate::adapters::events::EventPipeRx);

impl ConnectionEvents {
    /// Next event in order, or `None` once the stream has ended.
    pub async fn next(&mut self) -> Option<TransportEvent> {
        self.0.next().await
    }
}

/// Create a connected event channel with the given data-plane capacity. The
/// adapter keeps the `EventSink` and returns the `ConnectionEvents` from
/// [`Connection::take_event_pipe`].
///
/// Capacity is a [`NonZeroUsize`](std::num::NonZeroUsize): a zero-capacity
/// data plane is a programming error (the bounded channel would panic at
/// construction), so it is rejected at the type level instead.
pub fn event_channel(capacity: std::num::NonZeroUsize) -> (EventSink, ConnectionEvents) {
    let (tx, rx) = crate::adapters::events::event_pipe(capacity.get());
    (EventSink(tx), ConnectionEvents(rx))
}

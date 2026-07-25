//! Stable extension SPI for implementing a custom transport protocol.
//!
//! Everything an out-of-crate adapter needs — and nothing that leaks an
//! internal type. Implement [`Connection`] (and [`Server`]
//! for accept/connect), drive the read loop by pushing [`TransportEvent`]s into
//! an `EventSink`, and hand the paired `ConnectionEvents` back from
//! [`Connection::take_event_pipe`]. To advertise the config to the builder,
//! implement [`ServerConfig`]/[`ClientConfig`] (and their object-safe
//! `DynServerConfig`/`DynClientConfig`).

pub use crate::connection::{Connection, Server, WriteCompletion};
pub use crate::packet::Packet;
pub use crate::protocol::adapter::{
    ClientConfig, ConfigError, DynClientConfig, DynProtocolConfig, DynServerConfig, ProtocolConfig,
    ServerConfig,
};
pub use crate::transport::limits::ConnectionLimits;
pub use crate::{CloseReason, ConnectionInfo, SessionId, TransportError, TransportEvent};

/// Producer half of a connection's event channel, held by the adapter's read
/// loop. Public wrapper over the internal bounded backbone — the data plane
/// backpressures, the control plane (close) never blocks.
#[derive(Debug)]
pub struct EventSink(pub(crate) crate::adapters::events::EventPipe);

impl EventSink {
    /// Deliver a data-plane event with backpressure. Returns `false` when the
    /// consumer is gone — the adapter should stop reading.
    pub async fn deliver(&self, event: TransportEvent) -> bool {
        self.0.deliver(event).await
    }

    /// Emit a droppable diagnostic (e.g. a send confirmation). Never blocks;
    /// under load the diagnostic is dropped, never a data event.
    pub fn diagnostic(&self, event: TransportEvent) {
        self.0.diagnostic(event)
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

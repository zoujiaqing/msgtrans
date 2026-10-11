//! Client event stream.
//!
//! The broadcast-based `EventStream`/`PacketStream`/`ConnectionStream`/
//! `StreamFactory` machinery from 1.x is gone: the data path is the bounded
//! per-connection event backbone, and a client has exactly one consumer, so
//! there is nothing to fan out. The only stream type is [`ClientEvents`].

/// Client event stream — the single consumer of a client's events.
///
/// Backed by a bounded queue rather than a broadcast channel: a client has one
/// connection and one consumer, so there is nothing to fan out to, and a
/// bounded queue backpressures a slow consumer instead of silently skipping
/// events the way a lagging broadcast receiver would.
pub struct ClientEvents {
    inner: tokio::sync::mpsc::Receiver<crate::event::ClientEvent>,
}

impl ClientEvents {
    pub(crate) fn new(receiver: tokio::sync::mpsc::Receiver<crate::event::ClientEvent>) -> Self {
        Self { inner: receiver }
    }

    /// Receive the next event, or `None` once the client has shut down.
    pub async fn next(&mut self) -> Option<crate::event::ClientEvent> {
        self.inner.recv().await
    }

    /// Receive an event if one is already queued, without waiting.
    pub fn try_next(&mut self) -> Option<crate::event::ClientEvent> {
        self.inner.try_recv().ok()
    }
}

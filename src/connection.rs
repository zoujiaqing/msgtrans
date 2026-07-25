use crate::transport::request_registry::RespondClaim;
use crate::{command::ConnectionInfo, error::TransportError, packet::Packet, SessionId};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::oneshot;

/// The SINGLE send entry point, split off the connection object so the send
/// path never holds the transport's connection lock (see
/// [`Connection::writer`]).
///
/// There is deliberately no parallel `send()` — the fire-and-forget tier is
/// [`WriteCompletion::detached`], so enqueue, write receipt, cancellation and
/// close all flow through one ownership model.
#[async_trait]
pub trait ConnectionWriter: Send + Sync {
    /// Enqueue a packet carrying its write completion.
    ///
    /// Contract: the completion must be resolved with the REAL write result —
    /// `complete(Ok(()))` only after the bytes reached the socket (or its OS
    /// buffer), `complete(Err(..))` when the write failed. An implementation
    /// that can no longer write must DROP the completion (dropping reports
    /// failure by construction); it must never invent an `Ok`.
    ///
    /// This only enqueues, and may await backpressure while the outbound queue
    /// is full — which is safe precisely because no lock is held here.
    async fn send_with_completion(
        &self,
        packet: Packet,
        completion: WriteCompletion,
    ) -> Result<(), TransportError>;
}

/// The completion of one write: the single ownership handle for "what
/// happened to this packet". Stable SPI type — adapters receive it with
/// every enqueue and are the ONLY party that may resolve it, from their
/// write loop, with the real write result.
///
/// Resolution cannot be lost: `complete()` reports the outcome to both the
/// optional observer (a caller awaiting the receipt) and the optional
/// respond claim (the request registry's `Responding` entry). If the
/// completion is dropped instead — queue entry discarded, connection died,
/// enqueue future cancelled — the observer's channel closes (a connection
/// error to the awaiter) and the claim's own drop guard records a send
/// failure. Observers are pure observers: cancelling a caller's future never
/// strands registry state.
#[derive(Debug)]
pub struct WriteCompletion {
    observer: Option<oneshot::Sender<Result<(), TransportError>>>,
    claim: Option<RespondClaim>,
}

impl WriteCompletion {
    pub(crate) fn new(
        observer: Option<oneshot::Sender<Result<(), TransportError>>>,
        claim: Option<RespondClaim>,
    ) -> Self {
        Self { observer, claim }
    }

    /// A completion nobody observes: the fire-and-forget tier of the single
    /// send SPI. Enqueue success is the only signal the caller gets.
    pub fn detached() -> Self {
        Self {
            observer: None,
            claim: None,
        }
    }

    /// True when resolving this completion would inform nobody.
    pub fn is_detached(&self) -> bool {
        self.observer.is_none() && self.claim.is_none()
    }

    /// Resolve with the actual write outcome. Adapters MUST call this from
    /// the write loop after the socket write; a completion they cannot write
    /// must be dropped (which reports failure) — never completed with a
    /// made-up Ok.
    pub fn complete(mut self, result: Result<(), TransportError>) {
        if let Some(claim) = self.claim.take() {
            claim.resolve(result.is_ok());
        }
        if let Some(tx) = self.observer.take() {
            let _ = tx.send(result);
        }
    }
}

/// Unified connection interface - the single abstraction for all protocols
///
/// In v2.0, adapters implement this trait directly — no wrapper types needed.
#[async_trait]
pub trait Connection: Send + Sync + std::any::Any {
    /// Hand out this connection's write handle.
    ///
    /// The handle is cheap to clone and independent of the connection object,
    /// which is what keeps the transport's connection lock OFF the send path:
    /// the transport takes the lock only long enough to validate the generation
    /// and clone the writer, then releases it and awaits the (possibly
    /// backpressured) enqueue. Holding the lock across that wait would let one
    /// saturated queue park reconnect/close/shutdown behind every sender.
    ///
    /// Implementations return a handle over their outbound queue sender; it
    /// must stay bound to THIS connection, so a writer captured before a
    /// reconnect can never write onto the replacement connection.
    fn writer(&self) -> Arc<dyn ConnectionWriter>;

    /// Close connection
    async fn close(&mut self) -> Result<(), TransportError>;

    /// Get session ID
    fn session_id(&self) -> SessionId;

    /// Set session ID
    fn set_session_id(&mut self, session_id: SessionId);

    /// Get connection information
    fn connection_info(&self) -> ConnectionInfo;

    /// Check if connection is active
    fn is_connected(&self) -> bool;

    /// Flush send buffer
    async fn flush(&mut self) -> Result<(), TransportError>;

    /// Take the connection's event channel (single consumer, take-once). The
    /// transport drains it: all queued data events, then exactly one
    /// `ConnectionClosed`. Return the [`crate::spi::ConnectionEvents`] paired
    /// with the [`crate::spi::EventSink`] the read loop pushes into.
    fn take_event_pipe(&mut self) -> Option<crate::spi::ConnectionEvents>;

    /// Apply the frame decode policy to this connection. **Required**: there is
    /// no default, because a silent no-op default let external adapters ignore
    /// `Strict` and keep delivering undecodable frames as raw one-way messages.
    /// An adapter whose framing cannot honor the policy must still implement
    /// this — either enforcing it or documenting/erroring that it is fixed.
    fn set_frame_policy(&self, policy: crate::packet::FramePolicy);
}

/// Unified server interface - Accept new connections
#[async_trait]
pub trait Server: Send + Sync {
    /// Accept new connection
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError>;

    /// Get server bind address
    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError>;

    /// Shutdown server
    async fn shutdown(&mut self) -> Result<(), TransportError>;
}

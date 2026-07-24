use crate::transport::request_registry::RespondClaim;
use crate::{command::ConnectionInfo, error::TransportError, packet::Packet, SessionId};
use async_trait::async_trait;
use tokio::sync::oneshot;

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
    /// The SINGLE send entry point: enqueue a packet carrying its write
    /// completion. There is deliberately no parallel `send()` — the
    /// fire-and-forget tier is [`WriteCompletion::detached`], so enqueue,
    /// write receipt, cancellation and close all flow through one ownership
    /// model.
    ///
    /// Contract: the completion must be resolved with the REAL write result —
    /// `complete(Ok(()))` only after the bytes reached the socket (or its OS
    /// buffer), `complete(Err(..))` when the write failed. An implementation
    /// that can no longer write must DROP the completion (dropping reports
    /// failure by construction); it must never invent an Ok. This method only
    /// enqueues: callers reach it through a connection lock and await the
    /// completion's observer AFTER releasing it, or one slow write would
    /// serialize every other sender.
    async fn send_with_completion(
        &mut self,
        packet: Packet,
        completion: WriteCompletion,
    ) -> Result<(), TransportError>;

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

    /// Set the frame decode policy for this connection. Adapters that support it
    /// (WebSocket, QUIC) override this; others (e.g. TCP, which is always strict
    /// on a malformed first packet) keep the default no-op.
    fn set_frame_policy(&self, _policy: crate::packet::FramePolicy) {}
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

/// Connection factory - Create client connections
#[async_trait]
pub trait ConnectionFactory: Send + Sync {
    /// Establish connection
    async fn connect(&self) -> Result<Box<dyn Connection>, TransportError>;
}

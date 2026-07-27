//! Session Actor Module
//!
//! Implements per-connection actor model for high-throughput message processing.
//! Each connection gets its own dedicated queue and worker task, eliminating
//! global contention and enabling natural backpressure.
//!
//! Architecture:
//! ```text
//! Connection → flume(bounded) → SessionActor → SessionHandler
//!                     ↑ also receives Send/Close commands from TransportServer
//! ```

use crate::adapters::outbound::SEND_QUEUE_WAIT;

/// Lifecycle deadline for inbound requests: bounds how long an unanswered
/// request stays tracked (the timeout scanner reaps Pending entries).
use crate::transport::request_registry::REQUEST_LIFECYCLE_TIMEOUT;
use crate::transport::request_registry::{MarkResult, RequestRegistry, RequestToken};
use crate::{
    command::ConnectionInfo, event::TransportEvent, packet::Packet,
    transport::transport::Transport, SessionId,
};
use async_trait::async_trait;
use flume::{bounded, Receiver, Sender};
use std::sync::Arc;

/// Messages that flow through the actor's mailbox.
///
/// Combines inbound events (from the connection reader) and outbound commands
/// (from TransportServer's send path) into a single channel, eliminating the
/// need for the server to lock the connection Mutex on sends.
#[derive(Debug)]
pub enum ActorMessage {
    /// Inbound: a transport event from the connection reader task
    InboundEvent(TransportEvent),
    /// Outbound: send a packet through this actor's connection (fire-and-forget)
    Send(Packet),
    /// Outbound: send a packet and notify the caller of the result
    SendWithReply {
        packet: Packet,
        reply: tokio::sync::oneshot::Sender<Result<(), crate::TransportError>>,
    },
}

/// Session handler trait - business layer implements this to receive messages
///
/// This replaces the old `subscribe_events()` pattern with a direct callback model.
/// Each session has its own handler invocation, no global fan-out.
#[async_trait]
pub trait SessionHandler: Send + Sync + 'static {
    /// Called when a NON-request message is received from a session
    /// (one-way messages, unmatched responses).
    ///
    /// # Arguments
    /// * `session_id` - The session that sent the message
    /// * `packet` - The received packet
    /// * `sender` - Sender to send messages back to this session
    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender);

    /// Called when a REQUEST is received from a session. REQUIRED — requests
    /// carry an obligation to answer, and the consuming [`Responder`] is the
    /// only way to answer it: `respond(self)` is write-confirmed,
    /// `respond_detached(self)` is fire-and-forget, dropping it leaves the
    /// request to the lifecycle machinery (timeout / session-close drain).
    async fn on_request(&self, session_id: SessionId, request: Packet, responder: Responder);

    /// Called when a session is established.
    ///
    /// Runs before any `on_message`/`on_request` for this session, so the
    /// handler can set up per-session state without racing the first inbound
    /// packet.
    ///
    /// This is a **notification, not an admission decision**: it returns `()`
    /// and hands out no close handle, so returning early does NOT reject the
    /// connection. To drop an unwanted peer, close its session explicitly
    /// (e.g. `TransportServer::close_session`) from here or from the first
    /// message; the connection stays open until you do.
    async fn on_connected(&self, session_id: SessionId, info: ConnectionInfo) {
        let _ = (session_id, info); // Default: no-op
    }

    /// Called once a packet has been handed to the connection for sending.
    ///
    /// **Best-effort / droppable.** Send confirmations travel on the
    /// diagnostic channel, which is deliberately dropped under load so that a
    /// backlog of confirmations can never displace real data events. Never
    /// treat a missing call as "the message was not sent", and never use this
    /// as a delivery ledger — use the write-confirmed send paths
    /// (`TransportServer::send`, `Responder::respond`) when you need proof.
    async fn on_message_sent(&self, session_id: SessionId, message_id: u32) {
        let _ = (session_id, message_id); // Default: no-op
    }

    /// Called when a session is closed
    async fn on_disconnected(&self, session_id: SessionId, reason: crate::error::CloseReason) {
        let _ = (session_id, reason); // Default: no-op
    }

    /// Called when a transport error occurs
    async fn on_error(&self, session_id: SessionId, error: crate::TransportError) {
        let _ = (session_id, error); // Default: no-op
    }
}

/// Sender for sending messages back to a session
///
/// This is provided to the handler for every message, allowing the handler
/// to send responses or other messages back to the session.
#[derive(Clone)]
pub struct SessionSender {
    session_id: SessionId,
    transport: Arc<Transport>,
}

impl SessionSender {
    pub(crate) fn new(session_id: SessionId, transport: Arc<Transport>) -> Self {
        Self {
            session_id,
            transport,
        }
    }

    /// Send a ONE-WAY message to this session, **write-confirmed**: it returns
    /// only once the bytes reached the socket.
    ///
    /// There is deliberately no raw `send(Packet)` here. Accepting a
    /// caller-built packet allowed a `Request` with an id the registry never
    /// allocated; that id could collide with a tracked request and its response
    /// would complete the WRONG waiter. Requests go through
    /// `TransportServer::request*`, responses through [`Responder`].
    ///
    /// Bound to this session's connection generation: after a reconnect the
    /// send fails rather than being written onto the replacement connection.
    pub async fn send_data(
        &self,
        data: impl Into<bytes::Bytes>,
    ) -> Result<(), crate::TransportError> {
        self.send_data_with_options(data, crate::transport::SendOptions::new())
            .await
    }

    /// One-way send with options (biz_type / ext_header), write-confirmed.
    pub async fn send_data_with_options(
        &self,
        data: impl Into<bytes::Bytes>,
        options: crate::transport::SendOptions,
    ) -> Result<(), crate::TransportError> {
        let packet = self.build_one_way(data, &options)?;
        self.transport
            .send_confirmed_with(packet, Some(self.session_id), None)
            .await
    }

    /// Enqueue a one-way message without waiting for the write: `Ok` means
    /// QUEUED, not written. The explicit throughput tier — a failure after
    /// enqueue is only visible through the connection's error/close events.
    pub async fn send_data_detached(
        &self,
        data: impl Into<bytes::Bytes>,
    ) -> Result<(), crate::TransportError> {
        let packet = self.build_one_way(data, &crate::transport::SendOptions::new())?;
        self.transport.send(packet).await
    }

    /// Build the one-way packet with a transport-allocated id, applying the
    /// caller's options. Compression happens inside `apply`, so a missing codec
    /// is an error rather than a mislabeled frame on the wire.
    fn build_one_way(
        &self,
        data: impl Into<bytes::Bytes>,
        options: &crate::transport::SendOptions,
    ) -> Result<Packet, crate::TransportError> {
        let mut packet = Packet::one_way(self.transport.next_oneway_id(), data);
        options.apply(&mut packet)?;
        Ok(packet)
    }

    /// Get the session ID
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

/// The obligation to answer ONE request, and the only way to do it.
///
/// Not `Clone`: exactly one responder exists per delivered request, minted by
/// the dispatch layer together with the request's unforgeable
/// `RequestToken`. `respond(self)` consumes it — a second response is a
/// compile error, not a runtime dedup. Dropping it without responding leaves
/// the request to the lifecycle machinery (timeout scan / session-close
/// drain), which is the correct outcome for a request the business chose to
/// ignore.
pub struct Responder {
    token: RequestToken,
    biz_type: u8,
    session_id: SessionId,
    transport: Arc<Transport>,
    registry: Arc<RequestRegistry>,
}

impl Responder {
    pub(crate) fn new(
        token: RequestToken,
        biz_type: u8,
        session_id: SessionId,
        transport: Arc<Transport>,
        registry: Arc<RequestRegistry>,
    ) -> Self {
        Self {
            token,
            biz_type,
            session_id,
            transport,
            registry,
        }
    }

    /// Respond, write-confirmed. `Ok(RespondOutcome::Written)` means the
    /// response bytes reached the transport; `AlreadyHandled` means the
    /// request is no longer live (timed out, session closed, or its id was
    /// reused after this token's registration ended — the token's generation
    /// check refuses cross-registration responses by construction).
    ///
    /// Cancellation-safe: the claim travels with the queued write, so
    /// wrapping this in `timeout`/`select!` cannot strand registry state.
    /// The send is bound to the request's connection generation: it can
    /// never be written onto a replacement connection.
    pub async fn respond(
        self,
        data: impl Into<bytes::Bytes>,
    ) -> Result<crate::event::RespondOutcome, crate::TransportError> {
        self.respond_with_options(data, crate::transport::SendOptions::new())
            .await
    }

    /// Respond with explicit [`crate::SendOptions`] — same guarantees as
    /// [`Self::respond`], plus response-side compression / ext header /
    /// `biz_type` override.
    ///
    /// Defaults preserve `respond`'s behaviour: `biz_type: None` inherits the
    /// request's `biz_type` (a response is an answer to that request, not a
    /// message of business type 0). A compression failure — including "the
    /// codec feature is not compiled in" — fails the respond instead of
    /// shipping a raw payload under a compressed header, and the claim is
    /// resolved as a send failure.
    pub async fn respond_with_options(
        self,
        data: impl Into<bytes::Bytes>,
        options: crate::transport::SendOptions,
    ) -> Result<crate::event::RespondOutcome, crate::TransportError> {
        let data: bytes::Bytes = data.into();
        if self.registry.begin_respond(&self.token) != MarkResult::Updated {
            return Ok(crate::event::RespondOutcome::AlreadyHandled);
        }
        // Claim created in the same poll that won begin_respond: every
        // cancellation/failure path from here resolves it.
        let claim = crate::transport::request_registry::RespondClaim::new(
            self.registry.clone(),
            self.token,
        );
        let mut response_packet =
            Packet::response_with_biz(self.token.request_id(), self.biz_type, data);
        if let Err(e) = options.apply_over(&mut response_packet, self.biz_type) {
            // The claim is alive; dropping it here records the send failure,
            // so the request never lingers as Responding.
            drop(claim);
            return Err(e);
        }
        self.transport
            .send_confirmed_with(response_packet, Some(self.session_id), Some(claim))
            .await
            .map(|()| crate::event::RespondOutcome::Written)
    }

    /// Respond without awaiting the outcome. The registry state still
    /// resolves truthfully (the claim rides the queued write); only the
    /// caller's visibility is sacrificed.
    pub fn respond_detached(self, data: impl Into<bytes::Bytes>) {
        let data: bytes::Bytes = data.into();
        tokio::spawn(async move {
            if let Err(e) = self.respond(data).await {
                tracing::debug!("[RESPOND] detached response send failed: {:?}", e);
            }
        });
    }

    /// Get the session ID
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Get the request message ID
    pub fn message_id(&self) -> u32 {
        self.token.request_id()
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        // Deterministic cleanup: if the handler dropped this responder without
        // answering, the inbound request is still `Pending`. Resolve it as
        // `Dropped` right now (generation-aware, so a reused id is never
        // touched) instead of leaking the entry until the timeout scanner.
        // A no-op once `respond`/`respond_detached` moved the entry past
        // `Pending` (`respond_detached` keeps `self` alive inside its spawned
        // task until the send resolves, so the state has already advanced).
        self.registry.abort_request_token(&self.token);
    }
}

/// Handle to a session actor, held by TransportServer
///
/// Contains only the sender side of the channel.
/// When this is dropped, the actor will shut down.
#[derive(Clone)]
pub struct SessionHandle {
    /// Channel to send messages (events + commands) to the actor
    pub(crate) tx: Sender<ActorMessage>,
    /// Reference to the transport (kept alive with the handle).
    #[allow(dead_code)]
    pub(crate) transport: Arc<Transport>,
}

impl SessionHandle {
    /// Forward an inbound transport event to the actor
    ///
    /// This will apply backpressure if the channel is full.
    /// Non-blocking event injection: used by shutdown to offer the actor a
    /// graceful close without parking on a full mailbox (cancellation follows
    /// either way).
    pub(crate) fn try_send_event(&self, event: TransportEvent) -> bool {
        self.tx.try_send(ActorMessage::InboundEvent(event)).is_ok()
    }

    pub async fn send_event(
        &self,
        event: TransportEvent,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<TransportEvent>> {
        self.tx
            .send_async(ActorMessage::InboundEvent(event))
            .await
            .map_err(|e| {
                // Extract the TransportEvent from the ActorMessage for the error
                match e.0 {
                    ActorMessage::InboundEvent(evt) => tokio::sync::mpsc::error::SendError(evt),
                    _ => unreachable!(),
                }
            })
    }

    /// Enqueue an actor message with bounded backpressure.
    ///
    /// Transient mailbox pressure is absorbed by waiting; a mailbox that stays
    /// full fails with a resource error rather than stalling the caller. See
    /// [`crate::adapters::outbound`] for the rationale behind this policy.
    async fn enqueue(&self, message: ActorMessage) -> Result<(), crate::TransportError> {
        match tokio::time::timeout(SEND_QUEUE_WAIT, self.tx.send_async(message)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(crate::TransportError::connection_error(
                "Actor channel closed",
                false,
            )),
            Err(_elapsed) => Err(crate::TransportError::resource_error(
                "session_actor_outbound_queue",
                self.tx.len(),
                self.tx.capacity().unwrap_or(DEFAULT_ACTOR_BUFFER_SIZE),
            )),
        }
    }

    /// Send a packet through the actor (fire-and-forget, no reply).
    pub async fn send_packet(&self, packet: Packet) -> Result<(), crate::TransportError> {
        self.enqueue(ActorMessage::Send(packet)).await
    }

    /// Send a packet through the actor and wait for the send result.
    pub async fn send_packet_with_reply(
        &self,
        packet: Packet,
    ) -> Result<(), crate::TransportError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.enqueue(ActorMessage::SendWithReply {
            packet,
            reply: reply_tx,
        })
        .await?;
        reply_rx.await.map_err(|_| {
            crate::TransportError::connection_error("Actor dropped before reply", false)
        })?
    }
}

/// Batch size for recv_many - optimal for reducing syscalls and scheduling overhead
const BATCH_SIZE: usize = 64;

/// Session Actor - processes events for a single connection
///
/// Each connection has exactly one SessionActor running in its own task.
/// The actor's mailbox receives both inbound events and outbound send commands,
/// ensuring all I/O for a connection is serialized through a single task.
///
/// Uses batch processing for high throughput:
/// - Reduces await/wake cycles by up to 64x
/// - Reduces channel lock contention
/// - Better CPU cache utilization
pub struct SessionActor {
    session_id: SessionId,
    transport: Arc<Transport>,
    rx: Receiver<ActorMessage>,
    handler: Arc<dyn SessionHandler>,
    connection_info: ConnectionInfo,
    inbound_registry: Option<Arc<RequestRegistry>>,
    /// Cooperative cancellation (server side): the session supervisor cancels
    /// this to force the actor out of its mailbox wait during shutdown.
    cancel: tokio_util::sync::CancellationToken,
}

impl SessionActor {
    /// Create a new session actor
    pub fn new(
        session_id: SessionId,
        transport: Arc<Transport>,
        rx: Receiver<ActorMessage>,
        handler: Arc<dyn SessionHandler>,
        connection_info: ConnectionInfo,
    ) -> Self {
        Self {
            session_id,
            transport,
            rx,
            handler,
            connection_info,
            inbound_registry: None,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Attach the session supervisor's cancellation token (internal).
    pub(crate) fn with_cancel(mut self, cancel: tokio_util::sync::CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Attach the inbound request registry (actor mode) for idempotent responses
    /// and shared lifecycle tracking. Internal, so the public `new` signature and
    /// `create_session_actor` stay unchanged.
    pub(crate) fn with_inbound_registry(
        mut self,
        inbound_registry: Option<Arc<RequestRegistry>>,
    ) -> Self {
        self.inbound_registry = inbound_registry;
        self
    }

    /// Run the actor's event loop with batch processing
    ///
    /// Processes both inbound events (from the connection reader) and outbound
    /// commands (send requests from TransportServer). This eliminates the need
    /// for external callers to lock the connection Mutex.
    pub async fn run(self) {
        tracing::debug!(
            "[ACTOR] SessionActor started for session {}",
            self.session_id
        );

        // Notify handler that session is connected
        self.handler
            .on_connected(self.session_id, self.connection_info.clone())
            .await;

        // Pre-allocate batch buffer to avoid repeated allocations
        let mut batch: Vec<ActorMessage> = Vec::with_capacity(BATCH_SIZE);

        // Create sender once, reuse for all messages (it's Clone + cheap)
        let sender = SessionSender::new(self.session_id, self.transport.clone());

        loop {
            batch.clear();

            // Batch receive: wait for first message, then drain up to BATCH_SIZE.
            // The cancellation arm lets the session supervisor force the actor
            // out of an idle mailbox wait during shutdown; a handler blocked
            // inside on_message is only reached once it returns here.
            let first = tokio::select! {
                _ = self.cancel.cancelled() => break,
                msg = self.rx.recv_async() => msg,
            };
            match first {
                Ok(first) => batch.push(first),
                Err(_) => break,
            }
            while batch.len() < BATCH_SIZE {
                match self.rx.try_recv() {
                    Ok(next) => batch.push(next),
                    Err(flume::TryRecvError::Empty) => break,
                    Err(flume::TryRecvError::Disconnected) => break,
                }
            }

            // Process batch
            let mut should_break = false;
            for msg in batch.drain(..) {
                // A terminal event (ConnectionClosed / Close) earlier in this
                // batch stops the actor: do NOT run the remaining commands
                // against a dead connection. Pending replies fail deterministically.
                if should_break {
                    if let ActorMessage::SendWithReply { reply, .. } = msg {
                        let _ = reply.send(Err(crate::TransportError::connection_error(
                            "session closed",
                            true,
                        )));
                    }
                    continue;
                }
                match msg {
                    ActorMessage::InboundEvent(event) => {
                        match event {
                            TransportEvent::MessageReceived(packet) => {
                                if packet.header.packet_type == crate::packet::PacketType::Request {
                                    // Register the request and mint its token +
                                    // consuming Responder in one place. A refused
                                    // registration (duplicate in-flight id from the
                                    // peer) is a protocol violation: there is no
                                    // honest way to answer it, so it is dropped.
                                    let Some(registry) = self.inbound_registry.clone() else {
                                        tracing::warn!(
                                            "[ACTOR] Request received without a registry; dropping (session: {})",
                                            self.session_id
                                        );
                                        continue;
                                    };
                                    let token = registry.register(
                                        packet.header.message_id,
                                        Some(self.session_id),
                                        packet.header.biz_type,
                                        REQUEST_LIFECYCLE_TIMEOUT,
                                    );
                                    match token {
                                        Some(token) => {
                                            let responder = Responder::new(
                                                token,
                                                packet.header.biz_type,
                                                self.session_id,
                                                self.transport.clone(),
                                                registry,
                                            );
                                            self.handler
                                                .on_request(self.session_id, packet, responder)
                                                .await;
                                        }
                                        None => {
                                            tracing::warn!(
                                                "[ACTOR] Duplicate in-flight request id {} from session {}; dropping",
                                                packet.header.message_id,
                                                self.session_id
                                            );
                                        }
                                    }
                                } else {
                                    // Direct handler call - no spawn, no extra allocation
                                    self.handler
                                        .on_message(self.session_id, packet, sender.clone())
                                        .await;
                                }
                            }
                            TransportEvent::MessageSent { packet_id } => {
                                self.handler
                                    .on_message_sent(self.session_id, packet_id)
                                    .await;
                            }
                            TransportEvent::ConnectionClosed { reason } => {
                                tracing::debug!(
                                    "[ACTOR] Session {} closed: {:?}",
                                    self.session_id,
                                    reason
                                );
                                self.handler.on_disconnected(self.session_id, reason).await;
                                should_break = true;
                            }
                            TransportEvent::TransportError { error } => {
                                tracing::warn!(
                                    "[ACTOR] Session {} error: {:?}",
                                    self.session_id,
                                    error
                                );
                                self.handler.on_error(self.session_id, error).await;
                            }
                            _ => {
                                tracing::trace!(
                                    "[ACTOR] Session {} received unhandled event",
                                    self.session_id
                                );
                            }
                        }
                    }
                    ActorMessage::Send(packet) => {
                        // Outbound send routed through actor — uses Transport::send
                        // which holds the Mutex internally, but now serialized per-connection.
                        if let Err(e) = self.transport.send(packet).await {
                            tracing::warn!(
                                "[ACTOR] Session {} send failed: {:?}",
                                self.session_id,
                                e
                            );
                        }
                    }
                    ActorMessage::SendWithReply { packet, reply } => {
                        // Write-confirmed: a "send that reports its result" (used
                        // by the server's request path) must report the WRITE
                        // result, not enqueue success — otherwise the server's
                        // response timeout could start before the request bytes
                        // reach the socket (ghost RPC). Bound to this session's
                        // generation so it can never write onto a replacement.
                        let result = self
                            .transport
                            .send_confirmed_with(packet, Some(self.session_id), None)
                            .await;
                        let _ = reply.send(result);
                    }
                }
            }

            if should_break {
                break;
            }
        }

        tracing::debug!(
            "[ACTOR] SessionActor stopped for session {}",
            self.session_id
        );
    }
}

/// Default buffer size for session actor channels.
///
/// Re-exported from [`crate::transport::limits::DEFAULT_MAILBOX_CAPACITY`] so
/// there is exactly ONE value: the builder's fallback and `ServerLimits`'
/// default used to be two different numbers.
pub use crate::transport::limits::DEFAULT_MAILBOX_CAPACITY as DEFAULT_ACTOR_BUFFER_SIZE;

/// Create a new session actor pair (handle + actor)
///
/// Returns the handle (for TransportServer) and the actor (to be spawned).
/// The channel carries `ActorMessage` — both inbound events and outbound commands.
pub fn create_session_actor(
    session_id: SessionId,
    transport: Arc<Transport>,
    handler: Arc<dyn SessionHandler>,
    connection_info: ConnectionInfo,
    buffer_size: usize,
) -> (SessionHandle, SessionActor) {
    let (tx, rx) = bounded(buffer_size);

    let handle = SessionHandle {
        tx,
        transport: transport.clone(),
    };

    let actor = SessionActor::new(session_id, transport, rx, handler, connection_info);

    (handle, actor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{config::TransportConfig, context::TransportContext};

    async fn test_handle(tx: Sender<ActorMessage>) -> SessionHandle {
        let context = TransportContext::new().await.unwrap();
        let transport = Arc::new(Transport::with_context(
            TransportConfig::default(),
            &context,
        ));
        SessionHandle { tx, transport }
    }

    /// A mailbox that stays full must fail with a resource error instead of
    /// stalling the caller indefinitely.
    #[tokio::test]
    async fn outbound_send_fails_when_actor_mailbox_stays_full() {
        let (tx, _rx) = bounded(1);
        tx.try_send(ActorMessage::Send(Packet::one_way(1, vec![1])))
            .unwrap();
        let handle = test_handle(tx).await;

        let error = handle
            .send_packet_with_reply(Packet::one_way(2, vec![2]))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            crate::TransportError::Resource { ref resource, current: 1, limit: 1 }
                if resource == "session_actor_outbound_queue"
        ));
    }

    /// The regression guard: a mailbox that is only briefly full must still
    /// accept the packet once the actor drains it.
    #[tokio::test]
    async fn transient_full_actor_mailbox_is_absorbed() {
        let (tx, rx) = bounded(1);
        tx.try_send(ActorMessage::Send(Packet::one_way(1, vec![1])))
            .unwrap();
        let handle = test_handle(tx).await;

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = rx.recv_async().await;
            // Hold the receiver so the channel stays connected.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });

        assert!(handle
            .send_packet(Packet::one_way(2, vec![2]))
            .await
            .is_ok());
    }
}

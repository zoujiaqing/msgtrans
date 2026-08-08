use crate::{
    connection::Connection,
    event::TransportEvent,
    transport::{
        config::TransportConfig, connection_state::ConnectionStateManager,
        context::TransportContext,
    },
    Packet, SessionId, TransportError,
};
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// One item of the client event queue: the event, the generation it came
/// from, and — for inbound requests — the unforgeable registration token the
/// respond path must present to the registry.
pub struct TaggedClientEvent {
    pub session_id: SessionId,
    pub(crate) token: Option<crate::transport::request_registry::RequestToken>,
    pub event: TransportEvent,
}

/// A connection bound to its generation. See `Transport::slot`.
struct ConnectionSlot {
    session_id: SessionId,
    connection: Box<dyn Connection>,
    /// This connection's write handle, captured once when the connection is
    /// installed. Senders clone it under the slot lock and then RELEASE the
    /// lock before awaiting the enqueue, so a saturated outbound queue can
    /// never park reconnect/close/shutdown behind the waiting senders.
    writer: Arc<dyn crate::connection::ConnectionWriter>,
}

/// Single connection transport abstraction — one instance per socket.
///
/// v1.3: Constructor is now synchronous. Heavy resources come from `TransportContext`
/// which is created once by the builder and shared across all Transport instances.
pub struct Transport {
    config: TransportConfig,
    /// The connection and its generation id live in ONE slot under ONE lock:
    /// validating the generation and taking/replacing the connection is a
    /// single atomic operation, so a stale close can never observe the old
    /// session while the new connection is already installed (the two-lock
    /// version had exactly that window).
    slot: Arc<Mutex<Option<ConnectionSlot>>>,
    state_manager: ConnectionStateManager,
    /// Client-facing event queue: bounded, single consumer (the client's
    /// forwarding task). Replaces the broadcast hop, whose one Lagged killed
    /// the forwarding task and silently ended all client event delivery.
    /// Items carry the source generation's SessionId: consumers (the client's
    /// request contexts in particular) must bind responses to the connection
    /// the request arrived on, not to "whatever connection is current".
    client_events_tx: mpsc::Sender<TaggedClientEvent>,
    client_events_rx: Arc<Mutex<Option<mpsc::Receiver<TaggedClientEvent>>>>,
    request_registry: Arc<crate::transport::request_registry::RequestRegistry>,
    /// Monotonic connection generation. Each set_connection bumps it; the
    /// per-connection pipe consumer only acts while its epoch is current, so
    /// a stale connection's delayed events (in particular ConnectionClosed ->
    /// abort_all) cannot cancel the replacement connection's requests.
    connection_epoch: Arc<std::sync::atomic::AtomicU64>,
}
/// Fallback lifecycle deadline for waiter-based requests. The real timeout is
/// enforced by the caller (tokio::time::timeout); this only bounds the entry if
/// the caller forgets to remove it.
/// Fallback deadline recorded for a waiter-based request whose caller did not
/// name one. Waiters are NOT scheduled into the timeout wheel — the caller's
/// own `tokio::time::timeout` is the real deadline — so this only labels the
/// entry for diagnostics. It is deliberately NOT the inbound lifecycle
/// constant: those are different policies, and merging them made every
/// outbound entry claim a 30s deadline no matter what the caller asked for.
const DEFAULT_WAITER_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
/// Lifecycle deadline for inbound (peer-initiated) requests on the client
/// side. Client transports run no timeout scanner, so this only labels the
/// entry; actual cleanup is the respond itself or the session-close drain,
/// which caps entry lifetime at the connection's lifetime.
use crate::transport::request_registry::REQUEST_LIFECYCLE_TIMEOUT as INBOUND_REQUEST_TIMEOUT;
impl Transport {
    /// Create Transport from a shared context (synchronous — no global singletons).
    pub(crate) fn with_context(config: TransportConfig, ctx: &TransportContext) -> Self {
        // Both event hops take the SAME configured capacity. This one was a
        // fixed 8192 while the outer TransportClient queue read the config, so
        // `ClientLimits::pipe_capacity(1)` still let 8192 events pile up here
        // and backpressure never reached the adapter.
        let (client_events_tx, client_events_rx) =
            mpsc::channel(config.connection_limits.pipe_capacity());
        let _ = ctx;
        Self {
            config,
            slot: Arc::new(Mutex::new(None)),
            state_manager: ConnectionStateManager::new(),
            client_events_tx,
            client_events_rx: Arc::new(Mutex::new(Some(client_events_rx))),
            request_registry: Arc::new(crate::transport::request_registry::RequestRegistry::new()),
            connection_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Send data packet through the underlying connection (single-lock hot
    /// path). Fire-and-forget tier of the single send SPI: the enqueue result
    /// is the only signal.
    pub async fn send(&self, packet: Packet) -> Result<(), TransportError> {
        // Clone the writer under the lock, then DROP the lock: the enqueue
        // below may wait on backpressure, and holding the slot lock across
        // that wait would serialize reconnect/close/shutdown behind senders.
        let writer = {
            let guard = self.slot.lock().await;
            match guard.as_ref() {
                Some(slot) => slot.writer.clone(),
                None => return Err(TransportError::connection_error("Not connected", false)),
            }
        };
        writer
            .send_with_completion(packet, crate::connection::WriteCompletion::detached())
            .await
    }

    /// Confirmed send bound to a connection generation and (optionally) a
    /// respond claim.
    ///
    /// - `expected_session`: validated against the CURRENT slot's generation
    ///   under the slot lock, atomically with taking that generation's writer.
    ///   A response created against generation N is therefore enqueued on
    ///   generation N's queue or not at all — never onto generation N+1 after a
    ///   reconnect — even though the lock is released before the enqueue.
    /// - `claim`: moved into the [`WriteCompletion`] in the same poll that
    ///   created it, BEFORE the first await. From that point every path —
    ///   cancellation of this future, enqueue rejection, connection death,
    ///   the write itself — resolves the claim exactly once by construction.
    pub(crate) async fn send_confirmed_with(
        &self,
        packet: Packet,
        expected_session: Option<SessionId>,
        claim: Option<crate::transport::request_registry::RespondClaim>,
    ) -> Result<(), TransportError> {
        let (observer_tx, observer_rx) = tokio::sync::oneshot::channel();
        // Created before the first await: cancellation from here on drops the
        // completion, which reports failure to the claim and the observer.
        let completion = crate::connection::WriteCompletion::new(Some(observer_tx), claim);
        // Validate the generation and take the writer under the lock, then
        // release it. The captured writer belongs to the generation we just
        // validated, so the "never write onto a replacement connection"
        // guarantee is preserved WITHOUT holding the lock across the enqueue's
        // backpressure wait.
        let writer = {
            let guard = self.slot.lock().await;
            match guard.as_ref() {
                Some(slot) => {
                    if let Some(expected) = expected_session {
                        if slot.session_id != expected {
                            // `completion` drops here: the stale respond is
                            // recorded as a send failure, and crucially the
                            // packet is NOT written to the new generation.
                            return Err(TransportError::connection_error(
                                "connection replaced since the request was received",
                                false,
                            ));
                        }
                    }
                    slot.writer.clone()
                }
                None => {
                    return Err(TransportError::connection_error("Not connected", false));
                }
            }
        };
        writer.send_with_completion(packet, completion).await?;
        crate::adapters::outbound::await_receipt(observer_rx, "connection closed before write")
            .await
    }

    /// The request registry shared by this transport's request/respond paths.
    pub(crate) fn request_registry(
        &self,
    ) -> &Arc<crate::transport::request_registry::RequestRegistry> {
        &self.request_registry
    }

    /// Apply a frame decode policy to the underlying connection (if connected).
    pub(crate) async fn set_frame_policy(&self, policy: crate::packet::FramePolicy) {
        if let Some(slot) = self.slot.lock().await.as_ref() {
            slot.connection.set_frame_policy(policy);
        }
    }

    /// TARGET Core method: disconnect connection (graceful shutdown)
    pub async fn disconnect(&self) -> Result<(), TransportError> {
        if let Some(session_id) = self.current_session_id().await {
            self.close_session(session_id).await
        } else {
            Err(TransportError::connection_error("Not connected", false))
        }
    }

    /// Retire a dead generation: atomically remove the ConnectionSlot if it
    /// still belongs to `session_id` and mark the connection closed. Called
    /// from the pipe consumer's teardown after a peer-initiated close, so a
    /// dead connection is not left installed — without this, is_connected()
    /// kept answering true and the corpse was only discovered on the next
    /// send. A newer generation's slot is never touched.
    pub(crate) async fn retire_generation(&self, session_id: SessionId) {
        let taken = {
            let mut guard = self.slot.lock().await;
            match guard.as_ref() {
                Some(slot) if slot.session_id == session_id => guard.take(),
                _ => None,
            }
        };
        // The generation is closed either way — mark it even when its slot
        // was already replaced by a newer generation (which must not be
        // touched, but the OLD generation's state must still say closed).
        self.state_manager.mark_closed(session_id).await;
        if taken.is_some() {
            tracing::debug!(
                "[RETIRE] Generation {} slot removed after peer close",
                session_id
            );
        } else {
            tracing::debug!(
                "[RETIRE] Stale generation {} retired (slot already replaced)",
                session_id
            );
        }
    }

    /// TARGET Unified close method: graceful session shutdown
    pub async fn close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        // 1. Check if we can start closing
        if !self.state_manager.try_start_closing(session_id).await {
            tracing::debug!(
                "Session {} already closing or closed, skipping close logic",
                session_id
            );
            return Ok(());
        }

        tracing::info!("[CONN] Starting graceful session shutdown: {}", session_id);
        let failed_pending = self.request_registry.close_session_pending(session_id);
        if failed_pending > 0 {
            tracing::debug!(
                "[REQUEST] Failed {} pending requests during session {} shutdown",
                failed_pending,
                session_id
            );
        }

        // 2. Close the connection ONLY if it still belongs to this session.
        // Both locks are held across the check and the close, so a concurrent
        // set_connection (which takes the connection lock) cannot install a
        // replacement between them — a stale generation's close can never
        // reach the new generation's socket.
        // Atomically take the slot ONLY if it still belongs to this session;
        // the network-facing close then runs outside the state lock.
        let taken = {
            let mut guard = self.slot.lock().await;
            match guard.as_ref() {
                Some(slot) if slot.session_id == session_id => guard.take(),
                _ => None,
            }
        };
        if let Some(mut slot) = taken {
            match tokio::time::timeout(
                self.config.graceful_timeout,
                self.try_graceful_close(&mut *slot.connection),
            )
            .await
            {
                Ok(Ok(_)) => {
                    tracing::debug!("[SUCCESS] Session {} graceful close successful", session_id);
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        "[WARN] Session {} graceful close failed, forcing: {:?}",
                        session_id,
                        e
                    );
                    let _ = slot.connection.close().await;
                }
                Err(_) => {
                    tracing::warn!(
                        "[WARN] Session {} graceful close timeout, forcing",
                        session_id
                    );
                    let _ = slot.connection.close().await;
                }
            }
        } else {
            tracing::debug!(
                "[SKIP] Stale close for session {}: connection now belongs to a newer generation",
                session_id
            );
        }

        self.state_manager.mark_closed(session_id).await;
        tracing::info!("[SUCCESS] Session {} shutdown complete", session_id);
        Ok(())
    }

    pub async fn force_close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        if !self.state_manager.try_start_closing(session_id).await {
            tracing::debug!(
                "Session {} already closing or closed, skipping force close",
                session_id
            );
            return Ok(());
        }

        tracing::info!("[CONN] Force closing session: {}", session_id);
        let failed_pending = self.request_registry.close_session_pending(session_id);
        if failed_pending > 0 {
            tracing::debug!(
                "[REQUEST] Failed {} pending requests during session {} force close",
                failed_pending,
                session_id
            );
        }

        let taken = {
            let mut guard = self.slot.lock().await;
            match guard.as_ref() {
                Some(slot) if slot.session_id == session_id => guard.take(),
                _ => None,
            }
        };
        if let Some(mut slot) = taken {
            let _ = slot.connection.close().await;
        } else {
            tracing::debug!(
                "[SKIP] Stale force-close for session {}: connection now belongs to a newer generation",
                session_id
            );
        }

        self.state_manager.mark_closed(session_id).await;

        tracing::info!("[SUCCESS] Session {} force close complete", session_id);
        Ok(())
    }
    /// Try graceful close with timeout
    async fn try_graceful_close(&self, conn: &mut dyn Connection) -> Result<(), TransportError> {
        // Directly use underlying protocol close mechanism
        // Each protocol has its own close signal:
        // - QUIC: CONNECTION_CLOSE frame
        // - TCP: FIN packet
        // - WebSocket: Close frame
        tracing::debug!("[CONN] Using underlying protocol graceful close mechanism");
        conn.close().await
    }

    /// TARGET Core method: check connection status
    pub async fn is_connected(&self) -> bool {
        self.slot.lock().await.is_some()
    }

    /// TARGET Core method: get current session ID
    pub async fn current_session_id(&self) -> Option<SessionId> {
        self.slot.lock().await.as_ref().map(|s| s.session_id)
    }

    /// Set connection and start internal event consumer (used by TransportClient).
    ///
    /// Generates and returns the connection's SessionId: the id IS the
    /// monotonically increasing connection epoch, so every session-keyed
    /// effect (request registration, teardown, close) is generation-scoped by
    /// construction — a stale generation's cleanup can never touch its
    /// replacement, because they never share an id. Callers must use the
    /// returned id; nothing else may invent one.
    pub(crate) async fn set_connection(
        self: &Arc<Self>,
        mut connection: Box<dyn Connection>,
    ) -> SessionId {
        // Allocate the epoch INSIDE the slot lock so allocation order and
        // installation order cannot invert under concurrent calls (an
        // out-of-order install would make the newest generation judge itself
        // stale). pub(crate): the supervisor/protocol layer is the only
        // legitimate installer.
        let (session_id, event_pipe_opt, info) = {
            let mut guard = self.slot.lock().await;
            let epoch = self
                .connection_epoch
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let session_id = SessionId::new(epoch);
            connection.set_session_id(session_id);
            let event_pipe_opt = connection.take_event_pipe();
            let writer = connection.writer();
            let info = connection.connection_info();
            *guard = Some(ConnectionSlot {
                session_id,
                connection,
                writer,
            });
            (session_id, event_pipe_opt, info)
        };
        self.state_manager.add_connection(session_id);
        // Open request tracking for this session; the registry refuses
        // registrations for sessions that were never opened or already closed.
        self.request_registry.open_session(session_id);
        tracing::debug!("[SUCCESS] Transport connection set: {}", session_id);
        // Announce the connection. Until 2.0.0-alpha.3 NOTHING produced this
        // event, so `ClientEvent::Connected` never fired and the README/examples
        // waited for a message that could not arrive. It is emitted here, where
        // the connection (and its real ConnectionInfo) is installed, and it is
        // emitted on reconnects too — every generation announces itself.
        self.forward_client_event(
            session_id,
            crate::event::TransportEvent::ConnectionEstablished { info },
        )
        .await;
        if let Some(mut pipe) = event_pipe_opt {
            // Bounded backbone: single-consumer queue with backpressure; the
            // pipe ends with exactly one ConnectionClosed.
            //
            // Weak, not Arc: a strong reference here would cycle (task ->
            // Transport -> sender feeding this task), so dropping the client
            // would leak the task, socket, server session and permit forever.
            let this = Arc::downgrade(self);
            tokio::spawn(async move {
                tracing::debug!(
                    "[LISTEN] Transport event consumer started (pipe, session: {})",
                    session_id
                );
                while let Some(event) = pipe.next().await {
                    let Some(strong) = this.upgrade() else { break };
                    // All effects are keyed by this generation's session id, so
                    // even arbitrarily late execution cannot touch a newer
                    // generation. The epoch filter below only reduces stale
                    // data-event noise; correctness does not depend on it.
                    if strong
                        .connection_epoch
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != session_id.as_u64()
                    {
                        break;
                    }
                    strong.on_event(session_id, event).await;
                }
                let Some(strong) = this.upgrade() else { return };
                // Generation-scoped teardown: closes only THIS connection's
                // session, so even a late-running stale task cannot cancel the
                // replacement generation's requests (its session id differs).
                strong.retire_generation(session_id).await;
                let failed_pending = strong.request_registry.close_session_pending(session_id);
                if failed_pending > 0 {
                    tracing::debug!(
                        "[REQUEST] Failed {} pending requests after event pipe ended (session: {})",
                        failed_pending,
                        session_id
                    );
                }
                tracing::debug!(
                    "[LISTEN] Transport event consumer ended (pipe, session: {})",
                    session_id
                );
            });
        }
        session_id
    }

    /// Set connection without starting event consumer loop.
    ///
    /// Used by TransportServer which manages its own event routing
    /// (direct connection → SessionActor path, skipping redundant intermediate broadcast).
    pub async fn set_connection_no_consumer(
        &self,
        mut connection: Box<dyn Connection>,
        session_id: SessionId,
    ) {
        connection.set_session_id(session_id);
        let writer = connection.writer();
        *self.slot.lock().await = Some(ConnectionSlot {
            session_id,
            connection,
            writer,
        });
        self.state_manager.add_connection(session_id);
        tracing::debug!(
            "[SUCCESS] Transport connection set (no consumer): {}",
            session_id
        );
    }

    /// Get configuration
    pub fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Take the client event queue (single consumer, once). The queue spans
    /// reconnects: new connections' pipe consumers feed the same sender, and
    /// every item is tagged with the generation (SessionId) it came from.
    pub async fn get_event_stream(&self) -> Option<tokio::sync::mpsc::Receiver<TaggedClientEvent>> {
        self.client_events_rx.lock().await.take()
    }

    /// Send data packet and wait for response
    pub async fn request(&self, packet: Packet) -> Result<Packet, TransportError> {
        if packet.header.packet_type != crate::packet::PacketType::Request {
            return Err(TransportError::connection_error(
                "Not a Request packet",
                false,
            ));
        }

        // [FIX] Use client-set message_id instead of overriding it
        let client_message_id = packet.header.message_id;
        let session_id = self.current_session_id().await;
        let (rx, token) = match self.request_registry.try_register_waiter(
            client_message_id,
            session_id,
            packet.header.biz_type,
            DEFAULT_WAITER_DEADLINE,
        ) {
            Ok(pair) => pair,
            Err(_) => {
                return Err(TransportError::connection_error(
                    "Duplicate in-flight request id",
                    false,
                ))
            }
        };

        // RAII guard: any exit from here that is NOT an explicit disarm — an
        // error return, a timeout, OR the caller cancelling this future
        // (select!/abort/outer timeout) — aborts the waiter on drop, so a
        // cancelled request never leaks a pending entry (waiters are not in
        // the timeout wheel). The guard is bound to `token`, so aborting is
        // generation-checked: a cancelled OLD request cannot delete the waiter
        // of a NEW request that reused its message id.
        let mut guard = crate::transport::request_registry::OutboundRequestGuard::new(
            self.request_registry.clone(),
            token,
        )
        .armed();

        // Confirm the WRITE before starting the response timeout: the waiter is
        // already registered (so a fast response cannot be missed), and only
        // once the request bytes have reached the socket do we begin the
        // response deadline. This closes the ghost-RPC window where a request
        // stuck in a congested outbound queue could time out for the caller
        // and then still be written and executed at the peer.
        self.send_confirmed_with(packet, session_id, None).await?;
        let timeout_duration = std::time::Duration::from_secs(10);
        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(resp)) => {
                guard.disarm();
                Ok(resp)
            }
            Ok(Err(_)) => Err(TransportError::connection_error("Connection closed", true)),
            Err(_) => Err(TransportError::timeout_error("request", timeout_duration)),
        }
    }

    /// TARGET Unified event handling entry point - complete unpacking and send user-friendly events at this layer
    /// Handle one event from a connection, keyed by the session (generation)
    /// that produced it — never by "whatever session is current now", which a
    /// delayed event from an old connection could otherwise poison.
    pub(crate) async fn on_event(
        &self,
        source_session: SessionId,
        event: crate::event::TransportEvent,
    ) {
        match event {
            crate::event::TransportEvent::MessageReceived(packet) => {
                tracing::debug!(
                    "[TARGET] Transport::on_event processing message packet: ID={}, type={:?}",
                    packet.header.message_id,
                    packet.header.packet_type
                );

                match packet.header.packet_type {
                    crate::packet::PacketType::Response => {
                        let id = packet.header.message_id;
                        tracing::debug!(
                            "[RECV] Processing response packet: ID={}, type={:?}, biz_type={}",
                            id,
                            packet.header.packet_type,
                            packet.header.biz_type
                        );
                        let session_id = Some(source_session);
                        let completed =
                            self.request_registry
                                .complete_waiter(session_id, id, packet.clone());
                        tracing::debug!(
                            "[PROC] Response packet processing result: ID={}, completed={}",
                            id,
                            completed
                        );
                        if !completed {
                            tracing::warn!("[WARN] Response packet ID={} not found in request tracker, may be timeout or duplicate", id);
                            // Forward unmatched responses so higher layers can handle them.
                            self.forward_client_event(
                                source_session,
                                crate::event::TransportEvent::MessageReceived(packet),
                            )
                            .await;
                        }
                    }

                    crate::packet::PacketType::Request => {
                        let id = packet.header.message_id;
                        tracing::debug!("[PROC] Received request packet, creating unified TransportContext: ID={}, type={:?}", id, packet.header.packet_type);

                        // Register the inbound request under ITS generation so the
                        // respond path gets the full claimed lifecycle (single
                        // responder, duplicate refusal, drain on session close).
                        let Some(token) = self.request_registry.register(
                            id,
                            Some(source_session),
                            packet.header.biz_type,
                            INBOUND_REQUEST_TIMEOUT,
                        ) else {
                            // Registration refused: the peer reused an id that is
                            // still in flight, or the session is closing. DROP it.
                            //
                            // Forwarding it anyway (as 2.0.0-alpha.3 did) handed the
                            // consumer a request with no registration token, and the
                            // respond path then skipped the registry entirely and
                            // wrote a SECOND real response for the same request id.
                            // The registry is the only arbiter of "one response per
                            // request", so an unregistrable request must never reach
                            // the consumer.
                            tracing::warn!(
                                "[PROC] Dropping unregistrable inbound request (duplicate in-flight id or closing session): ID={}, session={}",
                                id,
                                source_session
                            );
                            return;
                        };
                        tracing::debug!(
                            "[SEND] Sending unified MessageReceived event (Request): ID={}",
                            id
                        );
                        self.forward_client_event_with_token(
                            source_session,
                            Some(token),
                            crate::event::TransportEvent::MessageReceived(packet),
                        )
                        .await;
                    }

                    crate::packet::PacketType::OneWay => {
                        tracing::debug!(
                            "[RECV] Processing one-way message packet: ID={}, type={:?}",
                            packet.header.message_id,
                            packet.header.packet_type
                        );

                        // Already normalized (decompressed) at the dispatch
                        // boundary above, so the consumer gets plaintext.
                        self.forward_client_event(
                            source_session,
                            crate::event::TransportEvent::MessageReceived(packet),
                        )
                        .await;
                    }
                }
            }
            crate::event::TransportEvent::ConnectionClosed { reason } => {
                // Clean this generation's pending requests FIRST: forwarding
                // below can park on a saturated client queue, and waiters must
                // not stay pending behind it. Scoped to the source session, so
                // a stale close can never cancel a newer generation.
                let failed = self.request_registry.close_session_pending(source_session);
                if failed > 0 {
                    tracing::debug!(
                        "[REQUEST] Closed {} pending requests for session {} on ConnectionClosed",
                        failed,
                        source_session
                    );
                }
                // Retire the dead slot BEFORE forwarding: the forward below can
                // park on a saturated client queue, and until retirement runs
                // is_connected() would keep reporting a corpse as live. The
                // pipe teardown's retire remains as the idempotent fallback
                // for abnormal endings that never produce this event.
                self.retire_generation(source_session).await;
                self.forward_client_event(
                    source_session,
                    crate::event::TransportEvent::ConnectionClosed { reason },
                )
                .await;
            }
            // Forward other events directly
            _ => {
                tracing::trace!("[SEND] Forwarding other event: {:?}", event);
                self.forward_client_event(source_session, event).await;
            }
        }
    }

    /// Forward an event to the client's bounded queue, tagged with the source
    /// generation. Backpressures the pipe consumer (and through it the adapter
    /// and socket); if the client dropped its receiver, events are discarded —
    /// there is no consumer to lose them.
    async fn forward_client_event(&self, source_session: SessionId, event: TransportEvent) {
        self.forward_client_event_with_token(source_session, None, event)
            .await;
    }

    async fn forward_client_event_with_token(
        &self,
        source_session: SessionId,
        token: Option<crate::transport::request_registry::RequestToken>,
        event: TransportEvent,
    ) {
        let _ = self
            .client_events_tx
            .send(TaggedClientEvent {
                session_id: source_session,
                token,
                event,
            })
            .await;
    }

    /// Send data packet and wait for response (with options)
    pub async fn request_with_options(
        &self,
        data: Bytes,
        options: super::RequestOptions,
    ) -> Result<Bytes, TransportError> {
        // Always allocate a unique, strictly monotonic, per-session request id.
        // A caller-chosen (or wrapped) id could be reused while a previous
        // request with the same id still had a response in flight; the peer
        // echoes the id on the wire with no generation, so a late old response
        // would be delivered to the new caller. Exhaustion fails the request
        // instead of restarting the id space under live requests.
        let session_id = self.current_session_id().await;
        let Some(message_id) = self.request_registry.next_request_id(session_id) else {
            return Err(TransportError::resource_error(
                "request_id_space",
                u32::MAX as usize,
                u32::MAX as usize,
            ));
        };

        // Create request packet; options.apply performs any compression and
        // fails loudly rather than shipping a mislabeled body.
        let mut packet = crate::packet::Packet::request(message_id, data.clone());
        options.send.apply(&mut packet)?;

        // Register request tracking
        let session_id = self.current_session_id().await;
        let (rx, token) = match self.request_registry.try_register_waiter(
            message_id,
            session_id,
            packet.header.biz_type,
            // The caller's REAL deadline, so the registry entry does not claim
            // one the caller never asked for.
            options.timeout.unwrap_or(DEFAULT_WAITER_DEADLINE),
        ) {
            Ok(pair) => pair,
            Err(_) => {
                return Err(TransportError::connection_error(
                    "Duplicate in-flight request id",
                    false,
                ))
            }
        };

        tracing::debug!(
            "[SEND] Sending request: message_id={}, biz_type={}, timeout={:?}",
            message_id,
            packet.header.biz_type,
            options.timeout
        );

        // RAII guard: cancelling this future / any error exit aborts the waiter
        // on drop (waiters are not in the timeout wheel), so a cancelled
        // request never leaks a pending entry. Bound to `token`, so the abort is
        // generation-checked against message-id reuse.
        let mut guard = crate::transport::request_registry::OutboundRequestGuard::new(
            self.request_registry.clone(),
            token,
        )
        .armed();

        // Confirm the write before the response deadline (see `request`): no
        // ghost RPCs from a request that timed out while queued and was then
        // written.
        self.send_confirmed_with(packet, session_id, None).await?;

        tracing::debug!(
            "[WAIT] Waiting for response: message_id={}, timeout={:?}",
            message_id,
            options.timeout
        );

        // Wait for response (with custom timeout)
        let timeout_duration = options
            .timeout
            .unwrap_or(std::time::Duration::from_secs(10));
        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(resp)) => {
                guard.disarm();
                tracing::debug!(
                    "[SUCCESS] Received response: message_id={}, biz_type={}, payload_len={}",
                    message_id,
                    resp.header.biz_type,
                    resp.payload.len()
                );
                // Already normalized at the dispatch boundary: the waiter
                // receives a plaintext packet.
                Ok(resp.payload)
            }
            Ok(Err(_)) => {
                tracing::warn!("[WARN] Response channel closed: message_id={}", message_id);
                Err(TransportError::connection_error("Connection closed", true))
            }
            Err(_) => {
                tracing::warn!(
                    "[WARN] Request timeout: message_id={}, timeout={:?}",
                    message_id,
                    timeout_duration
                );
                Err(TransportError::timeout_error("request", timeout_duration))
            }
        }
    }

    /// Id for a ONE-WAY message (wrapping is harmless: nothing matches on it).
    pub(crate) fn next_oneway_id(&self) -> u32 {
        self.request_registry.next_oneway_id()
    }

    /// Id for an OUTBOUND request on the current session. `None` means the
    /// session's request-id space is exhausted (or the session is gone): the
    /// caller must fail the request rather than reuse an id.
    pub(crate) async fn next_request_id(&self) -> Option<u32> {
        let session_id = self.current_session_id().await;
        self.request_registry.next_request_id(session_id)
    }

    /// Send a one-way message with options, **write-confirmed** (every public
    /// `send*` is confirmed; the detached tier is explicitly named).
    pub async fn send_with_options(
        &self,
        data: Bytes,
        options: super::SendOptions,
    ) -> Result<(), TransportError> {
        // One-way: nothing matches on this id, so a wrapping counter is fine.
        let message_id = self.request_registry.next_oneway_id();

        // Create one-way message packet; options.apply performs any compression.
        let mut packet = crate::packet::Packet::one_way(message_id, data.clone());
        options.apply(&mut packet)?;

        // Write-confirmed, like every other public `send*` (the detached tier
        // is explicitly named `send_detached`).
        let session_id = self.current_session_id().await;
        self.send_confirmed_with(packet, session_id, None).await
    }
}

impl Clone for Transport {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            slot: self.slot.clone(),
            state_manager: self.state_manager.clone(),
            client_events_tx: self.client_events_tx.clone(),
            client_events_rx: self.client_events_rx.clone(),
            request_registry: self.request_registry.clone(),
            connection_epoch: self.connection_epoch.clone(),
        }
    }
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport")
            .field("connected", &"<async>")
            .field("session_id", &"<async>")
            .finish()
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockConn {
        closed: Arc<AtomicBool>,
        session_id: SessionId,
        sent: Arc<std::sync::atomic::AtomicUsize>,
    }

    /// Writer half of `MockConn`: counts sends and confirms immediately.
    struct MockWriter {
        sent: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::connection::ConnectionWriter for MockWriter {
        async fn send_with_completion(
            &self,
            _packet: Packet,
            completion: crate::connection::WriteCompletion,
        ) -> Result<(), TransportError> {
            self.sent.fetch_add(1, Ordering::SeqCst);
            completion.complete(Ok(()));
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl Connection for MockConn {
        fn writer(&self) -> Arc<dyn crate::connection::ConnectionWriter> {
            Arc::new(MockWriter {
                sent: self.sent.clone(),
            })
        }
        async fn close(&mut self) -> Result<(), TransportError> {
            self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn set_frame_policy(&self, _policy: crate::packet::FramePolicy) {}
        fn session_id(&self) -> SessionId {
            self.session_id
        }
        fn set_session_id(&mut self, session_id: SessionId) {
            self.session_id = session_id;
        }
        fn connection_info(&self) -> crate::command::ConnectionInfo {
            crate::command::ConnectionInfo::default()
        }
        fn is_connected(&self) -> bool {
            !self.closed.load(std::sync::atomic::Ordering::SeqCst)
        }
        async fn flush(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
        fn take_event_pipe(&mut self) -> Option<crate::spi::ConnectionEvents> {
            None
        }
    }

    fn mock(closed: &Arc<AtomicBool>) -> Box<dyn Connection> {
        mock_counting(closed, &Arc::new(std::sync::atomic::AtomicUsize::new(0)))
    }

    fn mock_counting(
        closed: &Arc<AtomicBool>,
        sent: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Box<dyn Connection> {
        Box::new(MockConn {
            closed: closed.clone(),
            session_id: SessionId::new(0),
            sent: sent.clone(),
        })
    }

    /// The generation invariants the ConnectionSlot exists for: distinct ids
    /// per generation, a stale close cannot touch the replacement connection
    /// BOTH client-event hops must take the configured capacity.
    ///
    /// This inner queue was a fixed 8192 while the outer `TransportClient`
    /// queue read the config, so `ClientLimits::pipe_capacity(1)` still let
    /// 8192 events accumulate here and backpressure never reached the adapter.
    #[tokio::test]
    async fn the_inner_event_queue_takes_the_configured_capacity() {
        let ctx = TransportContext::new().await.expect("ctx");
        let mut config = TransportConfig::default();
        config.connection_limits = crate::transport::limits::ClientLimits::new()
            .pipe_capacity(7)
            .connection_limits();

        let transport = Transport::with_context(config, &ctx);
        assert_eq!(
            transport.client_events_tx.max_capacity(),
            7,
            "a small pipe must actually be small on this hop too"
        );
    }

    /// (validate-and-take is one atomic slot operation), and a current close
    /// closes exactly its own connection.
    #[tokio::test]
    async fn stale_close_cannot_touch_the_replacement_connection() {
        let ctx = TransportContext::new().await.expect("ctx");
        let transport = Arc::new(Transport::with_context(TransportConfig::default(), &ctx));

        let closed1 = Arc::new(AtomicBool::new(false));
        let closed2 = Arc::new(AtomicBool::new(false));

        let id1 = transport.set_connection(mock(&closed1)).await;
        let id2 = transport.set_connection(mock(&closed2)).await;
        assert_ne!(id1, id2, "each generation must get a distinct session id");
        assert_eq!(transport.current_session_id().await, Some(id2));

        // Stale close: generation 1's connection was already replaced (and
        // dropped); closing id1 must not close generation 2's connection.
        transport.close_session(id1).await.expect("stale close ok");
        assert!(
            !closed2.load(Ordering::SeqCst),
            "stale close reached the replacement connection"
        );
        assert_eq!(
            transport.current_session_id().await,
            Some(id2),
            "replacement must remain installed after a stale close"
        );

        // Current close: closes exactly its own connection.
        transport
            .close_session(id2)
            .await
            .expect("current close ok");
        assert!(
            closed2.load(Ordering::SeqCst),
            "current close must close its connection"
        );
        assert_eq!(transport.current_session_id().await, None);
    }

    /// A response created against generation N must never be written onto
    /// generation N+1: send_confirmed_with validates the generation under the
    /// slot lock, atomically with the enqueue, and resolves the claim as a
    /// send failure without touching the new connection.
    #[tokio::test]
    async fn stale_generation_respond_is_refused_and_resolves_the_claim() {
        let ctx = TransportContext::new().await.expect("ctx");
        let transport = Arc::new(Transport::with_context(TransportConfig::default(), &ctx));
        let closed = Arc::new(AtomicBool::new(false));
        let sent2 = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let id1 = transport.set_connection(mock(&closed)).await;
        let id2 = transport
            .set_connection(mock_counting(&closed, &sent2))
            .await;

        // A request arrived on generation 1 and its respond was claimed.
        let registry = transport.request_registry().clone();
        registry.open_session(id1);
        use crate::transport::request_registry::{MarkResult, RespondClaim};
        let token = registry
            .register(9, Some(id1), 0, std::time::Duration::from_secs(5))
            .expect("registers");
        assert_eq!(registry.begin_respond(&token), MarkResult::Updated);
        let claim = RespondClaim::new(registry.clone(), token);

        // The respond runs after the reconnect: refused, nothing written.
        let err = transport
            .send_confirmed_with(
                Packet::response(9, b"late".to_vec()),
                Some(id1),
                Some(claim),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::Connection { .. }));
        assert_eq!(
            sent2.load(Ordering::SeqCst),
            0,
            "stale respond must not reach the replacement connection"
        );
        // The claim resolved as a send failure — no immortal Responding entry.
        assert_eq!(
            registry.get_state(
                Some(id1),
                9,
                crate::transport::request_registry::RequestDirection::Inbound
            ),
            None
        );
        assert_eq!(registry.counters_snapshot().response_send_failed_total, 1);

        // The current generation still works, write-confirmed.
        transport
            .send_confirmed_with(Packet::response(10, b"ok".to_vec()), Some(id2), None)
            .await
            .expect("current generation must accept confirmed sends");
        assert_eq!(sent2.load(Ordering::SeqCst), 1);
    }

    /// A stale response can only complete its own generation's waiter: request
    /// ids are keyed by (session, id, direction), and each generation has a
    /// unique session id.
    #[tokio::test]
    async fn stale_response_cannot_complete_the_new_generations_request() {
        let ctx = TransportContext::new().await.expect("ctx");
        let transport = Arc::new(Transport::with_context(TransportConfig::default(), &ctx));
        let closed = Arc::new(AtomicBool::new(false));

        let id1 = transport.set_connection(mock(&closed)).await;
        let id2 = transport.set_connection(mock(&closed)).await;

        // Generation 2 registers request 42.
        let (mut rx, _token) = transport
            .request_registry
            .try_register_waiter(42, Some(id2), 0, std::time::Duration::from_secs(5))
            .expect("registers");

        // A delayed response from generation 1 with the same message id must
        // not complete it — exercised through the REAL on_event path, so a
        // regression that falls back to current_session_id is caught here.
        let stale = Packet::response(42, b"stale".to_vec());
        transport
            .on_event(id1, crate::event::TransportEvent::MessageReceived(stale))
            .await;
        assert!(rx.try_recv().is_err(), "waiter must still be pending");

        // The correct generation's response completes it, same path.
        let good = Packet::response(42, b"good".to_vec());
        transport
            .on_event(id2, crate::event::TransportEvent::MessageReceived(good))
            .await;
        assert_eq!(rx.try_recv().expect("delivered").payload, b"good"[..]);
    }

    /// The connection lock must NOT be held across the enqueue await.
    ///
    /// A writer that parks forever stands in for a saturated outbound queue.
    /// While a sender is parked inside `send_with_completion`, a lifecycle
    /// operation that needs the slot lock must still make progress. Before the
    /// `ConnectionWriter` split the lock was held across the enqueue, so this
    /// deadlocked (the assert below timed out).
    #[tokio::test(flavor = "multi_thread")]
    async fn lifecycle_lock_is_free_while_a_sender_is_parked() {
        /// Never resolves: models an outbound queue that never drains.
        struct ParkedWriter;

        #[async_trait::async_trait]
        impl crate::connection::ConnectionWriter for ParkedWriter {
            async fn send_with_completion(
                &self,
                _packet: Packet,
                completion: crate::connection::WriteCompletion,
            ) -> Result<(), TransportError> {
                std::future::pending::<()>().await;
                drop(completion);
                Ok(())
            }
        }

        struct ParkedConn {
            session_id: SessionId,
        }

        #[async_trait::async_trait]
        impl Connection for ParkedConn {
            fn writer(&self) -> Arc<dyn crate::connection::ConnectionWriter> {
                Arc::new(ParkedWriter)
            }
            async fn close(&mut self) -> Result<(), TransportError> {
                Ok(())
            }
            fn set_frame_policy(&self, _policy: crate::packet::FramePolicy) {}
            fn session_id(&self) -> SessionId {
                self.session_id
            }
            fn set_session_id(&mut self, session_id: SessionId) {
                self.session_id = session_id;
            }
            fn connection_info(&self) -> crate::command::ConnectionInfo {
                crate::command::ConnectionInfo::default()
            }
            fn is_connected(&self) -> bool {
                true
            }
            async fn flush(&mut self) -> Result<(), TransportError> {
                Ok(())
            }
            fn take_event_pipe(&mut self) -> Option<crate::spi::ConnectionEvents> {
                None
            }
        }

        let ctx = TransportContext::new().await.expect("ctx");
        let transport = Arc::new(Transport::with_context(TransportConfig::default(), &ctx));
        let session_id = transport
            .set_connection(Box::new(ParkedConn {
                session_id: SessionId::new(0),
            }))
            .await;

        // Park a sender inside the enqueue, forever.
        let sender = {
            let transport = transport.clone();
            tokio::spawn(async move { transport.send(Packet::one_way(1, b"x".to_vec())).await })
        };
        // Let it actually reach the enqueue.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // A lifecycle operation needing the SAME slot lock must not block.
        let closed = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            transport.close_session(session_id),
        )
        .await;
        assert!(
            closed.is_ok(),
            "close_session blocked behind a sender parked in the enqueue: the \
             connection lock is being held across the await"
        );

        sender.abort();
    }

    /// A duplicate inbound request id (one already in flight) must be DROPPED,
    /// never forwarded.
    ///
    /// 2.0.0-alpha.3 forwarded it with `token: None`, and `ClientRequest::respond`
    /// then took a bypass branch that skipped the registry entirely and wrote a
    /// SECOND real response for the same request id. The registry is the only
    /// arbiter of "exactly one response per request", so an unregistrable
    /// request must not reach the consumer at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_inbound_request_is_dropped_not_forwarded() {
        let ctx = TransportContext::new().await.expect("ctx");
        let transport = Arc::new(Transport::with_context(TransportConfig::default(), &ctx));
        let closed = Arc::new(AtomicBool::new(false));
        let session_id = transport.set_connection(mock(&closed)).await;

        // Take the client event queue so we can observe what is forwarded.
        let mut events = transport
            .get_event_stream()
            .await
            .expect("client events taken once");

        // set_connection announces the connection first; drain it.
        let established = events.try_recv().expect("ConnectionEstablished forwarded");
        assert!(
            matches!(
                established.event,
                crate::event::TransportEvent::ConnectionEstablished { .. }
            ),
            "a new connection must announce itself (ClientEvent::Connected)"
        );

        // First inbound request with id 7: registers and is forwarded WITH a token.
        let req = Packet::request(7, b"one".to_vec());
        transport
            .on_event(
                session_id,
                crate::event::TransportEvent::MessageReceived(req),
            )
            .await;
        let first = events.try_recv().expect("first request forwarded");
        assert!(
            first.token.is_some(),
            "a registered request must carry its registration token"
        );

        // SAME id while the first is still in flight: registration is refused.
        let dup = Packet::request(7, b"two".to_vec());
        transport
            .on_event(
                session_id,
                crate::event::TransportEvent::MessageReceived(dup),
            )
            .await;
        assert!(
            events.try_recv().is_err(),
            "a duplicate in-flight request id must be dropped, not forwarded \
             (forwarding it lets the respond path bypass the registry and emit \
             a second response for the same id)"
        );
    }
}

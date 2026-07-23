use crate::{
    command::TransportStats,
    transport::{
        config::TransportConfig,
        connection_state::ConnectionStateManager,
        lockfree::LockFreeHashMap,
        session_actor::{
            create_session_actor, SessionHandle, SessionHandler, DEFAULT_ACTOR_BUFFER_SIZE,
        },
    },
    Packet, SessionId, TransportError,
};
/// Server-side transport layer implementation
///
/// Provides multi-protocol server support, managing sessions and connections.
///
/// ## Architecture
///
/// Every connection is driven by its own actor — there is no event bus:
/// ```text
/// Connection → flume(bounded) → SessionActor → SessionHandler
/// ```
///
/// Because the mailbox is bounded, a handler that falls behind slows down only
/// its own connection. A fan-out bus would instead drop messages for every
/// subscriber once one of them lagged.
use std::sync::Arc;
const PHASE_IDLE: u8 = 0;
const PHASE_STARTING: u8 = 1;
const PHASE_RUNNING: u8 = 2;
const PHASE_SHUTTING_DOWN: u8 = 3;
const PHASE_STOPPED: u8 = 4;

/// Fail-fast guard carried by every infra task: if the task ends while the
/// infra token is NOT cancelled, the exit is abnormal (cancellation is the
/// only legitimate way out) — flag the failure and cancel the siblings so the
/// server cannot keep running degraded.
struct InfraFailFast {
    cancel: tokio_util::sync::CancellationToken,
    failed: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for InfraFailFast {
    fn drop(&mut self) {
        if !self.cancel.is_cancelled() {
            self.failed.store(true, std::sync::atomic::Ordering::SeqCst);
            self.cancel.cancel();
        }
    }
}

const LISTENER_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
const DEFAULT_REQUEST_LIFECYCLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// TransportServer - multi-protocol server
///
/// Design goals:
/// - Multi-protocol support
/// - High-concurrency connection management
/// - One actor per connection, with backpressure instead of message loss
pub struct TransportServer {
    config: TransportConfig,
    /// Shared context for creating Transport instances (no global singletons)
    context: Arc<crate::transport::context::TransportContext>,
    transports: Arc<LockFreeHashMap<SessionId, Arc<crate::transport::transport::Transport>>>,
    session_handles: Arc<LockFreeHashMap<SessionId, SessionHandle>>,
    session_id_generator: Arc<std::sync::atomic::AtomicU64>,
    stats: Arc<LockFreeHashMap<SessionId, TransportStats>>,
    protocol_configs:
        std::collections::HashMap<String, Box<dyn crate::protocol::adapter::DynServerConfig>>,
    state_manager: ConnectionStateManager,
    request_registry: Arc<crate::transport::request_registry::RequestRegistry>,
    session_handler: Arc<dyn SessionHandler>,
    actor_buffer_size: usize,
    frame_policy: crate::packet::FramePolicy,
    /// Hard cap on concurrent sessions across all protocols, enforced with a
    /// semaphore so concurrent accept loops (TCP/WS/QUIC) cannot race past the
    /// limit the way a len() check could. A connection accepted at capacity is
    /// closed immediately, before a Transport or actor is allocated; the permit
    /// lives inside the session's actor and is released when the actor ends.
    connection_permits: Arc<tokio::sync::Semaphore>,
    /// Configured cap, kept for logging only (the semaphore is the enforcer).
    max_connections: usize,
    /// Root cancellation: shutdown cancels it; every session and the infra
    /// token are children, so one cancel reaches everything.
    root_cancel: tokio_util::sync::CancellationToken,
    /// Infra (listeners + scanner) cancellation — child of root, so shutdown
    /// stops it too, while stop() can stop accepting without killing sessions.
    infra_cancel: tokio_util::sync::CancellationToken,
    /// SINGLE owner of every session-supervisor task. Session handles never
    /// enter a cancellable public future: shutdown only cancels and then
    /// observes `tracker.wait()` (cancel-safe, repeatable). Tracker emptiness
    /// IS the proof that every actor+pump ended and every permit returned.
    session_tracker: tokio_util::task::TaskTracker,
    /// Per-session cancel tokens (metadata only — no handles live here).
    session_cancels: Arc<dashmap::DashMap<SessionId, tokio_util::sync::CancellationToken>>,
    /// SINGLE owner of the real listener/scanner tasks. serve() only spawns
    /// into it and then OBSERVES — cancelling serve() cannot detach a
    /// listener, because serve() never holds their ownership.
    infra_tracker: tokio_util::task::TaskTracker,
    /// Owned watcher that publishes Stopped only after the infra tracker has
    /// ACTUALLY drained (real joins), never on a guess.
    infra_supervisor: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Set by the fail-fast guard when any infra task ends abnormally.
    infra_failed: Arc<std::sync::atomic::AtomicBool>,
    /// Startup error captured by the owned startup task for serve() to return.
    serve_error: Arc<std::sync::Mutex<Option<TransportError>>>,
    /// Admission gate: closed at shutdown start; add_session refuses (and a
    /// racer that slipped past the gate self-cancels on its re-check), so no
    /// session can be inserted after a shutdown report returns.
    admission_open: Arc<std::sync::atomic::AtomicBool>,
    /// Server lifecycle phase (single source of truth, CAS-transitioned):
    /// 0 Idle, 1 Starting, 2 Running, 3 ShuttingDown, 4 Stopped. The watch
    /// only NOTIFIES phase changes; it is never the truth.
    server_phase: Arc<std::sync::atomic::AtomicU8>,
    phase_notify: Arc<tokio::sync::watch::Sender<u8>>,
    /// Serializes shutdown owners; concurrent/repeat shutdowns queue here and
    /// re-run the idempotent flow (instant once everything is drained).
    shutdown_lock: Arc<tokio::sync::Mutex<()>>,
}

impl TransportServer {
    /// Create a server.
    ///
    /// Every connection is driven by its own [`SessionActor`], which invokes
    /// `handler` for that session's messages and lifecycle. This is the only
    /// mode: there is no fan-out event bus, so a slow consumer applies
    /// backpressure to its own connection instead of silently dropping
    /// messages for everyone.
    ///
    /// [`SessionActor`]: crate::transport::SessionActor
    pub async fn new(
        config: TransportConfig,
        protocol_configs: std::collections::HashMap<
            String,
            Box<dyn crate::protocol::adapter::DynServerConfig>,
        >,
        handler: Arc<dyn SessionHandler>,
        buffer_size: Option<usize>,
    ) -> Result<Self, TransportError> {
        let ctx = Arc::new(crate::transport::context::TransportContext::new().await?);
        let root_cancel = tokio_util::sync::CancellationToken::new();
        let infra_cancel = root_cancel.child_token();

        let server = Self {
            config,
            context: ctx,
            transports: Arc::new(LockFreeHashMap::new()),
            session_handles: Arc::new(LockFreeHashMap::new()),
            session_id_generator: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            stats: Arc::new(LockFreeHashMap::new()),
            protocol_configs,
            state_manager: ConnectionStateManager::new(),
            request_registry: Arc::new(
                crate::transport::request_registry::RequestRegistry::new_with_start_id(10000),
            ),
            session_handler: handler,
            actor_buffer_size: buffer_size.unwrap_or(DEFAULT_ACTOR_BUFFER_SIZE),
            frame_policy: crate::packet::FramePolicy::Lenient,
            connection_permits: Arc::new(tokio::sync::Semaphore::new(
                tokio::sync::Semaphore::MAX_PERMITS,
            )),
            max_connections: usize::MAX,
            root_cancel,
            infra_cancel,
            session_tracker: tokio_util::task::TaskTracker::new(),
            session_cancels: Arc::new(dashmap::DashMap::new()),
            infra_tracker: tokio_util::task::TaskTracker::new(),
            infra_supervisor: Arc::new(std::sync::Mutex::new(None)),
            infra_failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            serve_error: Arc::new(std::sync::Mutex::new(None)),
            admission_open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            server_phase: Arc::new(std::sync::atomic::AtomicU8::new(PHASE_IDLE)),
            phase_notify: Arc::new(tokio::sync::watch::channel(PHASE_IDLE).0),
            shutdown_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        Ok(server)
    }

    /// Set the frame decode policy applied to accepted connections. Internal;
    /// used by TransportServerBuilder to keep the public new*() signatures stable.
    pub(crate) fn with_frame_policy(mut self, policy: crate::packet::FramePolicy) -> Self {
        self.frame_policy = policy;
        self
    }

    /// Set the concurrent-session cap. Internal; set via
    /// `TransportServerBuilder::max_connections`.
    pub(crate) fn with_max_connections(mut self, max: usize) -> Self {
        // Range-validated by TransportServerBuilder::build before we get here.
        self.connection_permits = Arc::new(tokio::sync::Semaphore::new(max));
        self.max_connections = max;
        self
    }

    /// [LOCKFREE] Send packet to specified session
    ///
    /// The packet is routed through the session actor's mailbox, which serializes
    /// sends per-connection and avoids Mutex contention on the Transport layer.
    pub async fn send_to_session(
        &self,
        session_id: SessionId,
        packet: Packet,
    ) -> Result<(), TransportError> {
        tracing::debug!(
            "[SEND] TransportServer sending packet to session {} (ID: {}, size: {} bytes)",
            session_id,
            packet.header.message_id,
            packet.payload.len()
        );

        // Route through the actor mailbox (lock-free hot path)
        if let Some(handle) = self.session_handles.get(&session_id) {
            match handle.send_packet_with_reply(packet).await {
                Ok(()) => {
                    tracing::debug!(
                        "[SUCCESS] Session {} send successful (via actor)",
                        session_id
                    );
                    return Ok(());
                }
                Err(e) => {
                    let error_msg = format!("{:?}", e);
                    if error_msg.contains("Broken pipe")
                        || error_msg.contains("Connection reset")
                        || error_msg.contains("Connection closed")
                        || error_msg.contains("ECONNRESET")
                        || error_msg.contains("EPIPE")
                        || error_msg.contains("Actor channel closed")
                        || error_msg.contains("Actor dropped")
                    {
                        tracing::warn!(
                            "[WARN] Session {} connection closed: {}",
                            session_id,
                            error_msg
                        );
                        let _ = self.remove_session(session_id).await;
                        return Err(TransportError::connection_error(
                            "Connection closed during send",
                            false,
                        ));
                    } else {
                        tracing::error!("[ERROR] Session {} send failed: {:?}", session_id, e);
                        return Err(e);
                    }
                }
            }
        }

        // Fallback for the rare case where the actor handle failed to register:
        // send directly through Transport rather than dropping the packet.
        if let Some(transport) = self.transports.get(&session_id) {
            if !transport.is_connected().await {
                tracing::warn!(
                    "[WARN] Session {} connection closed, skipping send",
                    session_id
                );
                let _ = self.remove_session(session_id).await;
                return Err(TransportError::connection_error("Connection closed", false));
            }

            match transport.send(packet).await {
                Ok(()) => {
                    tracing::debug!(
                        "[SUCCESS] Session {} send successful (TransportServer layer confirmation)",
                        session_id
                    );
                    Ok(())
                }
                Err(e) => {
                    tracing::error!("[ERROR] Session {} send failed: {:?}", session_id, e);

                    let error_msg = format!("{:?}", e);
                    if error_msg.contains("Broken pipe")
                        || error_msg.contains("Connection reset")
                        || error_msg.contains("Connection closed")
                        || error_msg.contains("ECONNRESET")
                        || error_msg.contains("EPIPE")
                    {
                        tracing::warn!(
                            "[WARN] Session {} connection closed: {}",
                            session_id,
                            error_msg
                        );
                        let _ = self.remove_session(session_id).await;
                        Err(TransportError::connection_error(
                            "Connection closed during send",
                            false,
                        ))
                    } else {
                        tracing::error!(
                            "[ERROR] Session {} send failed (non-connection error): {:?}",
                            session_id,
                            e
                        );
                        Err(e)
                    }
                }
            }
        } else {
            tracing::warn!(
                "[WARN] Session {} does not exist in connection mapping",
                session_id
            );
            Err(TransportError::connection_error("Session not found", false))
        }
    }

    /// [REQUEST] Send request to specified session and wait for response.
    ///
    /// Uses TransportServer's own request tracker so responses are matched
    /// consistently at the server layer.
    pub async fn request_to_session(
        &self,
        session_id: SessionId,
        packet: Packet,
    ) -> Result<Packet, TransportError> {
        tracing::debug!(
            "[REQUEST] TransportServer sending request to session {} (ID: {})",
            session_id,
            packet.header.message_id
        );

        if self.transports.get(&session_id).is_none() {
            tracing::warn!(
                "[WARN] Session {} does not exist in connection mapping",
                session_id
            );
            return Err(TransportError::connection_error("Session not found", false));
        }

        if packet.header.packet_type != crate::packet::PacketType::Request {
            return Err(TransportError::connection_error(
                "Not a Request packet",
                false,
            ));
        }

        let message_id = packet.header.message_id;
        let rx = match self.request_registry.try_register_waiter(
            message_id,
            Some(session_id),
            packet.header.biz_type,
            DEFAULT_REQUEST_LIFECYCLE_TIMEOUT,
        ) {
            Ok(rx) => rx,
            Err(_) => {
                return Err(TransportError::connection_error(
                    "Duplicate in-flight request id for this session",
                    false,
                ))
            }
        };

        if let Err(e) = self.send_to_session(session_id, packet).await {
            self.request_registry
                .abort_waiter(Some(session_id), message_id);
            tracing::error!(
                "[ERROR] Session {} request send failed: {:?}",
                session_id,
                e
            );
            return Err(e);
        }

        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(response)) => {
                tracing::debug!(
                    "[SUCCESS] Session {} received response (response ID: {})",
                    session_id,
                    response.header.message_id
                );
                Ok(response)
            }
            Ok(Err(_)) => {
                self.request_registry
                    .abort_waiter(Some(session_id), message_id);
                Err(TransportError::connection_error("Connection closed", true))
            }
            Err(_) => {
                self.request_registry
                    .abort_waiter(Some(session_id), message_id);
                Err(TransportError::timeout_error(
                    "server request",
                    std::time::Duration::from_secs(10),
                ))
            }
        }
    }

    /// [UNIFIED] Send byte data to specified session - unified API returns TransportResult
    pub async fn send(
        &self,
        session_id: SessionId,
        data: &[u8],
    ) -> Result<crate::event::TransportResult, TransportError> {
        let message_id = self.request_registry.next_message_id();
        let packet = crate::packet::Packet::one_way(message_id, data.to_vec());

        tracing::debug!(
            "TransportServer sending data to session {}: {} bytes (ID: {})",
            session_id,
            data.len(),
            message_id
        );

        match self.send_to_session(session_id, packet).await {
            Ok(()) => {
                // Send successful, return TransportResult
                Ok(crate::event::TransportResult::new_sent(
                    Some(session_id),
                    message_id,
                ))
            }
            Err(e) => Err(e),
        }
    }

    /// [UNIFIED] Send byte request to specified session and wait for response - unified API returns TransportResult
    pub async fn request(
        &self,
        session_id: SessionId,
        data: &[u8],
    ) -> Result<crate::event::TransportResult, TransportError> {
        let message_id = self.request_registry.next_message_id();
        let packet = crate::packet::Packet::request(message_id, data.to_vec());

        tracing::debug!(
            "TransportServer sending request to session {}: {} bytes (ID: {})",
            session_id,
            data.len(),
            message_id
        );

        match self.request_to_session(session_id, packet).await {
            Ok(response_packet) => {
                tracing::debug!(
                    "TransportServer received response from session {}: {} bytes (ID: {})",
                    session_id,
                    response_packet.payload.len(),
                    response_packet.header.message_id
                );
                // Request successful, return TransportResult containing response data
                Ok(crate::event::TransportResult::new_completed(
                    Some(session_id),
                    message_id,
                    response_packet.payload.clone(),
                ))
            }
            Err(e) => {
                if matches!(e, TransportError::Timeout { .. }) {
                    Ok(crate::event::TransportResult::new_timeout(
                        Some(session_id),
                        message_id,
                    ))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Add a session carrying its connection-cap permit. The permit is moved
    /// into the session's actor so capacity is released exactly when the actor
    /// ends — whichever side closed and however teardown was reached.
    ///
    /// Internal: every real session must come through the accept path, which is
    /// where the permit is acquired — a public entry point taking no permit
    /// would be a hole in the connection cap.
    async fn add_session_with_permit(
        &self,
        connection: Box<dyn crate::Connection>,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> SessionId {
        // [FIX] Use existing session ID from connection instead of generating new one
        let session_id = connection.session_id();
        let mut connection = connection;
        // Admission gate: once shutdown began, no new session may form. The
        // permit drops with this early return.
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let _ = connection.close().await;
            return session_id;
        }
        connection.set_frame_policy(self.frame_policy);
        // Captured before the connection is moved into Transport, so the actor can
        // hand it to `on_connected` ahead of any inbound message.
        let connection_info = connection.connection_info();

        let transport = Arc::new(crate::transport::transport::Transport::with_context(
            self.config.clone(),
            &self.context,
        ));

        // [FIX] Subscribe to event stream BEFORE setting connection
        // This ensures that when adapter's event loop starts, there's already a subscriber ready
        // This prevents message loss during the time window between event loop start and consumer loop start
        let event_pipe_opt = connection.take_event_pipe();

        // Server manages its own event routing, skip Transport's internal consumer
        transport
            .set_connection_no_consumer(connection, session_id)
            .await;

        // Insert into transport layer mapping
        if let Err(e) = self.transports.insert(session_id, transport.clone()) {
            tracing::error!(
                "[ERROR] Failed to register transport for session {}: {:?}",
                session_id,
                e
            );
            let _ = transport.disconnect().await;
            return session_id;
        }
        if let Err(e) = self.stats.insert(session_id, TransportStats::new()) {
            tracing::error!(
                "[ERROR] Failed to register stats for session {}: {:?}",
                session_id,
                e
            );
            let _ = self.transports.remove(&session_id);
            let _ = transport.disconnect().await;
            return session_id;
        }

        // Register connection state
        self.state_manager.add_connection(session_id);
        // Open request tracking; a session that was never opened (or already
        // closed) refuses all request registrations.
        self.request_registry.open_session(session_id);

        // Create this session's actor and keep a handle for event forwarding.
        let (handle, actor) = create_session_actor(
            session_id,
            transport.clone(),
            self.session_handler.clone(),
            connection_info,
            self.actor_buffer_size,
        );
        let child_cancel = self.root_cancel.child_token();
        let actor = actor
            .with_inbound_registry(Some(self.request_registry.clone()))
            .with_cancel(child_cancel.clone());
        let actor_task = match self.session_handles.insert(session_id, handle.clone()) {
            Ok(_) => Some((tokio::spawn(actor.run()), handle)),
            Err(e) => {
                tracing::error!(
                    "[ERROR] Failed to register session actor handle for {}: {:?}",
                    session_id,
                    e
                );
                None
            }
        };
        let (actor_task, actor_handle) = match actor_task {
            Some((t, h)) => (Some(t), Some(h)),
            None => (None, None),
        };

        // [LOOP] Pump the connection's transport events into this session's actor.
        // Bounded-backbone adapters hand over a single-consumer pipe (real
        // backpressure; ends with exactly one ConnectionClosed). Legacy
        // adapters still expose the broadcast facade until their migration.
        let pump_task = if let Some(mut pipe) = event_pipe_opt {
            let server_clone = self.clone();
            let pump_cancel = child_cancel.clone();
            tokio::spawn(async move {
                tracing::info!(
                    "[LISTENER] TransportServer pump started for session {} (bounded pipe)",
                    session_id
                );
                loop {
                    // The cancel arm matters for IDLE sessions: without it the
                    // pump parks in pipe.next() until the ADAPTER ends, and
                    // nothing would end an idle adapter during shutdown. On
                    // cancel, pump_teardown's remove_session drops the
                    // Transport -> connection -> adapter, closing the socket.
                    let event = tokio::select! {
                        _ = pump_cancel.cancelled() => None,
                        ev = pipe.next() => ev,
                    };
                    let Some(transport_event) = event else { break };
                    if let Some(handle) = &actor_handle {
                        if !server_clone
                            .pump_one_event(session_id, handle, transport_event)
                            .await
                        {
                            break;
                        }
                    }
                }
                server_clone.pump_teardown(session_id).await;
            })
        } else {
            tracing::warn!(
                "[WARN] Session {} unable to get event stream before connection setup",
                session_id
            );
            tokio::spawn(async {})
        };

        // Per-session supervisor: OWNS the actor + pump handles, joins both,
        // then (idempotently) cleans the session maps, releases the permit —
        // exactly when the whole session runtime is done, not earlier — and
        // removes its own entry. Shutdown joins whichever supervisors are
        // still running; normal session end needs no shutdown involvement.
        // Registration barrier: the supervisor may not begin until its cancel
        // token is registered, so "completes before registration" cannot
        // strand metadata. OWNERSHIP: the supervisor task lives in the
        // server's TaskTracker — the single owner. Its handle never enters a
        // cancellable public future; shutdown only cancels tokens and then
        // observes tracker.wait(). The task cleans the session maps and its
        // own token entry (metadata only), and the permit drops exactly when
        // it returns — which is exactly when the tracker stops counting it.
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        {
            let server = self.clone();
            let sup_cancel = child_cancel.clone();
            self.session_tracker.spawn(async move {
                let _ = started_rx.await;
                let _permit = permit;
                // Observe BOTH children concurrently. A JoinError (panic or
                // abort) from either one cancels the sibling immediately —
                // otherwise a panicked actor would leave the pump reading the
                // socket forever, pinning the session and its permit until a
                // global shutdown.
                let mut pump_task = pump_task;
                if let Some(mut actor) = actor_task {
                    tokio::select! {
                        r = &mut actor => {
                            if r.is_err() {
                                tracing::warn!(
                                    "[SUPERVISOR] Session {} actor ended abnormally; cancelling sibling",
                                    session_id
                                );
                                sup_cancel.cancel();
                            }
                            let _ = pump_task.await;
                        }
                        r = &mut pump_task => {
                            if r.is_err() {
                                tracing::warn!(
                                    "[SUPERVISOR] Session {} pump ended abnormally; cancelling sibling",
                                    session_id
                                );
                                sup_cancel.cancel();
                            }
                            let _ = actor.await;
                        }
                    }
                } else {
                    let _ = pump_task.await;
                }
                let _ = server.remove_session(session_id).await;
                server.session_cancels.remove(&session_id);
                tracing::debug!("[SUPERVISOR] Session {} fully finalized", session_id);
            });
        }
        self.session_cancels
            .insert(session_id, child_cancel.clone());
        let _ = started_tx.send(());

        // Admission re-check: a shutdown that started between the gate check
        // and this insert could not see this session. Self-cancel so it tears
        // down through the normal cascade; the supervisor releases the permit.
        if !self
            .admission_open
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            child_cancel.cancel();
        }

        tracing::info!(
            "[SUCCESS] TransportServer added session: {} (using Transport abstraction)",
            session_id
        );
        session_id
    }

    /// Remove session
    pub async fn remove_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        let closed_pending = self.request_registry.close_session_pending(session_id);
        if closed_pending > 0 {
            tracing::debug!(
                "[REQUEST] Session {} removed, closed {} pending requests",
                session_id,
                closed_pending
            );
        }

        if let Err(e) = self.transports.remove(&session_id) {
            tracing::warn!(
                "[WARN] Failed to remove transport for session {}: {:?}",
                session_id,
                e
            );
        }
        if let Err(e) = self.session_handles.remove(&session_id) {
            tracing::warn!(
                "[WARN] Failed to remove session actor handle for session {}: {:?}",
                session_id,
                e
            );
        }
        if let Err(e) = self.stats.remove(&session_id) {
            tracing::warn!(
                "[WARN] Failed to remove stats for session {}: {:?}",
                session_id,
                e
            );
        }
        self.state_manager.remove_connection(session_id);
        tracing::info!("[REMOVE] TransportServer removed session: {}", session_id);
        Ok(())
    }

    /// Publish a phase value: CAS-free store used only by the owner of the
    /// transition; the watch is notification-only.
    fn set_phase(&self, phase: u8) {
        self.server_phase
            .store(phase, std::sync::atomic::Ordering::SeqCst);
        let _ = self.phase_notify.send_replace(phase);
    }

    fn phase(&self) -> u8 {
        self.server_phase.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Handle one pumped transport event for a session: complete server-side
    /// request futures, give inbound requests lifecycle tracking, then forward
    /// to the actor. Returns false when the pump should stop (close forwarded,
    /// or the actor is gone).
    async fn pump_one_event(
        &self,
        session_id: SessionId,
        handle: &SessionHandle,
        transport_event: crate::event::TransportEvent,
    ) -> bool {
        if let crate::event::TransportEvent::MessageReceived(packet) = &transport_event {
            if packet.header.packet_type == crate::packet::PacketType::Response
                && self.request_registry.complete_waiter(
                    Some(session_id),
                    packet.header.message_id,
                    packet.clone(),
                )
            {
                return true;
            }
            // Register inbound requests so they get lifecycle treatment
            // (timeout scan, batch-fail on session close) and idempotent respond.
            if packet.header.packet_type == crate::packet::PacketType::Request {
                self.request_registry.register(
                    packet.header.message_id,
                    Some(session_id),
                    packet.header.biz_type,
                    DEFAULT_REQUEST_LIFECYCLE_TIMEOUT,
                );
            }
        }

        // ConnectionClosed is the definitive end-of-stream marker: forward it,
        // then stop.
        let is_close = matches!(
            transport_event,
            crate::event::TransportEvent::ConnectionClosed { .. }
        );
        if let Err(e) = handle.send_event(transport_event).await {
            tracing::warn!(
                "[WARN] Failed to forward event to actor for session {}: {:?}",
                session_id,
                e
            );
            return false;
        }
        !is_close
    }

    /// Shared pump teardown: fail this session's pending requests and reap its
    /// map entries. Peer-initiated closes end here without ever passing through
    /// close_session/force_close_session, so the reap must happen now —
    /// otherwise entries linger until a send fails, and with a max_connections
    /// cap they would pin capacity forever. (Server-initiated closes already
    /// removed them; remove_session is idempotent, the presence check just
    /// avoids warn noise.)
    async fn pump_teardown(&self, session_id: SessionId) {
        let closed = self.request_registry.close_session_pending(session_id);
        if closed > 0 {
            tracing::debug!(
                "[END] Session {} loop ended: closed {} pending requests",
                session_id,
                closed
            );
        }
        if self.transports.get(&session_id).is_some() {
            let _ = self.remove_session(session_id).await;
        }
    }

    /// Feed a server-initiated close into the session's actor mailbox.
    ///
    /// The actor stops after handling it, so a subsequent `ConnectionClosed` from
    /// the adapter finds no receiver — `on_disconnected` fires exactly once
    /// regardless of which side started the close.
    async fn notify_actor_closed(&self, session_id: SessionId, reason: crate::error::CloseReason) {
        if let Some(handle) = self.session_handles.get(&session_id) {
            let _ = handle
                .send_event(crate::event::TransportEvent::ConnectionClosed { reason })
                .await;
        }
    }

    /// [UNIFIED] Unified close method: graceful session close
    pub async fn close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        // 1. Check if close can be started
        if !self.state_manager.try_start_closing(session_id).await {
            tracing::debug!(
                "Session {} already closing or closed, skipping close logic",
                session_id
            );
            return Ok(());
        }

        tracing::info!(
            "[CLOSE] Starting graceful close for session: {}",
            session_id
        );

        // 2. Notify the handler through this session's actor (before cleanup), so
        //    `on_disconnected` always arrives on the same task as `on_message`.
        self.notify_actor_closed(session_id, crate::error::CloseReason::Normal)
            .await;

        // 3. Execute actual close logic
        self.do_close_session(session_id).await?;

        // 4. Mark as closed
        self.state_manager.mark_closed(session_id).await;

        // 5. Clean up session
        self.remove_session(session_id).await?;

        tracing::info!("[SUCCESS] Session {} close completed", session_id);
        Ok(())
    }

    /// [FORCE] Force close session
    pub async fn force_close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        // 1. Check if close can be started
        if !self.state_manager.try_start_closing(session_id).await {
            tracing::debug!(
                "Session {} already closing or closed, skipping force close",
                session_id
            );
            return Ok(());
        }

        tracing::info!("[FORCE] Force closing session: {}", session_id);

        // 2. Notify the handler through this session's actor
        self.notify_actor_closed(session_id, crate::error::CloseReason::Forced)
            .await;

        // 3. Immediately force close, no waiting
        if let Some(transport) = self.transports.get(&session_id) {
            let _ = transport.disconnect().await; // Ignore errors, close directly
        }

        // 4. Mark as closed
        self.state_manager.mark_closed(session_id).await;

        // 5. Clean up session
        self.remove_session(session_id).await?;

        tracing::info!("[SUCCESS] Session {} force close completed", session_id);
        Ok(())
    }

    /// [BATCH] Batch close all sessions
    pub async fn close_all_sessions(&self) -> Result<(), TransportError> {
        let session_ids = self.active_sessions().await;
        let total_sessions = session_ids.len();

        if total_sessions == 0 {
            tracing::info!("No active sessions to close");
            return Ok(());
        }

        tracing::info!(
            "[BATCH] Starting batch close of {} sessions",
            total_sessions
        );

        // Use graceful_timeout as total timeout for batch close
        let start_time = std::time::Instant::now();
        let timeout = self.config.graceful_timeout;

        let mut success_count = 0;
        let mut error_count = 0;

        for session_id in session_ids {
            // Check if timeout
            if start_time.elapsed() >= timeout {
                tracing::warn!(
                    "[WARN] Batch close timeout, remaining sessions will be force closed"
                );
                // Force close remaining sessions
                let _ = self.force_close_session(session_id).await;
                continue;
            }

            // Try graceful close
            match self.close_session(session_id).await {
                Ok(_) => success_count += 1,
                Err(e) => {
                    error_count += 1;
                    tracing::warn!("[WARN] Failed to close session {}: {:?}", session_id, e);
                }
            }
        }

        tracing::info!(
            "[SUCCESS] Batch close completed, success: {}, failed: {}",
            success_count,
            error_count
        );
        Ok(())
    }

    /// Internal method: execute actual close logic - through Transport abstraction
    async fn do_close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        if let Some(transport) = self.transports.get(&session_id) {
            // Try graceful close
            match tokio::time::timeout(self.config.graceful_timeout, transport.disconnect()).await {
                Ok(Ok(_)) => {
                    tracing::debug!("[SUCCESS] Session {} graceful close successful", session_id);
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        "[WARN] Session {} graceful close failed: {:?}",
                        session_id,
                        e
                    );
                    // Graceful close failed, but don't return error, continue cleanup
                }
                Err(_) => {
                    tracing::warn!("[WARN] Session {} graceful close timeout", session_id);
                    // Timeout, but don't return error, continue cleanup
                }
            }
        }

        Ok(())
    }

    /// Check if connection should ignore messages
    pub async fn should_ignore_messages(&self, session_id: SessionId) -> bool {
        self.state_manager.should_ignore_messages(session_id).await
    }

    /// Broadcast message to all sessions
    ///
    /// Uses fire-and-forget sends through each actor's mailbox for throughput.
    pub async fn broadcast(&self, packet: Packet) -> Result<(), TransportError> {
        let mut success_count = 0;
        let mut error_count = 0;

        // Fan out through the actor mailboxes (fire-and-forget for speed)
        let session_ids: Vec<SessionId> = self.session_handles.keys().unwrap_or_default();
        for session_id in session_ids {
            if let Some(handle) = self.session_handles.get(&session_id) {
                match handle.send_packet(packet.clone()).await {
                    Ok(()) => success_count += 1,
                    Err(e) => {
                        error_count += 1;
                        tracing::warn!(
                            "[WARN] Broadcast to session {} failed: {:?}",
                            session_id,
                            e
                        );
                    }
                }
            }
        }

        if error_count > 0 {
            tracing::warn!(
                "[WARN] Broadcast completed, success: {}, failed: {}",
                success_count,
                error_count
            );
        } else {
            tracing::info!("[SUCCESS] Broadcast completed, success: {}", success_count);
        }

        Ok(())
    }

    /// Get active session list
    pub async fn active_sessions(&self) -> Vec<SessionId> {
        self.transports.keys().unwrap_or_default()
    }

    /// Get session count
    pub async fn session_count(&self) -> usize {
        self.transports.len()
    }

    /// Generate new session ID
    fn generate_session_id(&self) -> SessionId {
        let id = self
            .session_id_generator
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SessionId(id)
    }

    /// Start server
    pub async fn serve(&self) -> Result<(), TransportError> {
        // One-shot lifecycle gate: only Idle -> Starting is allowed. A second
        // or concurrent serve(), or a serve after shutdown, is rejected here
        // BEFORE touching any shared state, so it cannot re-arm is_running or
        // publish a phase that masks the real instance.
        if self
            .server_phase
            .compare_exchange(
                PHASE_IDLE,
                PHASE_STARTING,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return Err(TransportError::config_error(
                "server",
                "serve() may run once: server is already starting, running, or stopped",
            ));
        }
        let _ = self.phase_notify.send_replace(PHASE_STARTING);

        // The ENTIRE startup (bind, listener spawn, run, drain) lives in one
        // OWNED task, installed synchronously before serve()'s first await —
        // so cancelling serve() at ANY point, including during a slow protocol
        // build, detaches nothing and cannot wedge the phase: the owned task
        // (or its guard) always drives the phase to Stopped.
        {
            let server = self.clone();
            let handle = tokio::spawn(async move {
                server.startup_and_supervise().await;
            });
            *self
                .infra_supervisor
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(handle);
        }

        // Observe (cancel-safe): wait for the owned task to publish Stopped.
        let mut phase_rx = self.phase_notify.subscribe();
        while self.phase() != PHASE_STOPPED {
            if phase_rx.changed().await.is_err() {
                break;
            }
        }
        if let Some(e) = self
            .serve_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            return Err(e);
        }
        if self.infra_failed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(TransportError::config_error(
                "server",
                "an infrastructure task failed; all siblings were stopped and joined",
            ));
        }

        tracing::info!("[STOP] TransportServer stopped");
        Ok(())
    }

    /// The OWNED startup + supervision body: binds every protocol (Phase A,
    /// interruptible by root cancellation), starts all accept loops (Phase B),
    /// publishes Running, waits for the infra tracker to drain, and publishes
    /// Stopped. A guard guarantees Stopped (and sibling teardown) even on
    /// panic — the phase can never wedge in Starting.
    async fn startup_and_supervise(&self) {
        struct StartupGuard(TransportServer);
        impl Drop for StartupGuard {
            fn drop(&mut self) {
                if self.0.phase() != PHASE_STOPPED {
                    self.0
                        .admission_open
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    self.0.root_cancel.cancel();
                    self.0.set_phase(PHASE_STOPPED);
                }
            }
        }
        let guard = StartupGuard(self.clone());
        let fail = |e: TransportError| {
            *self.serve_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
        };

        if self.protocol_configs.is_empty() {
            tracing::warn!("[WARN] No protocols configured, server cannot start listening");
            fail(TransportError::config_error(
                "protocols",
                "No protocols configured",
            ));
            return; // guard publishes Stopped
        }
        tracing::info!(
            "[START] Starting {} protocol servers",
            self.protocol_configs.len()
        );

        // Phase A: bind every endpoint, no accept loop running. Interruptible:
        // a shutdown during a slow build cancels the root token and we stop.
        let mut built = Vec::new();
        for (protocol_name, protocol_config) in &self.protocol_configs {
            let address = self.get_protocol_bind_address(protocol_config);
            tracing::info!(
                "[BIND] Protocol {} bind address: {}",
                protocol_name,
                address
            );
            let build = tokio::select! {
                _ = self.root_cancel.cancelled() => {
                    tracing::info!("[STOP] Startup cancelled during {} build", protocol_name);
                    fail(TransportError::config_error(
                        "server",
                        "startup cancelled by shutdown",
                    ));
                    return; // built drops -> bound endpoints close; guard -> Stopped
                }
                r = protocol_config.build_server_dyn() => r,
            };
            match build {
                Ok(server) => built.push((protocol_name.clone(), server)),
                Err(e) => {
                    tracing::error!("[ERROR] {} server build failed: {:?}", protocol_name, e);
                    fail(e);
                    return; // built drops; guard -> Stopped
                }
            }
        }

        // Phase B: start all accept loops together (tracker-owned).
        let mut listen_tasks = Vec::new();
        for (protocol_name, server) in built {
            match self
                .start_protocol_listener(server, protocol_name.clone())
                .await
            {
                Ok(task) => {
                    listen_tasks.push(task);
                    tracing::info!("[SUCCESS] {} listener started", protocol_name);
                }
                Err(e) => {
                    tracing::error!("[ERROR] {} listener start failed: {:?}", protocol_name, e);
                    self.admission_open
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    self.root_cancel.cancel();
                    for task in listen_tasks {
                        task.abort();
                        let _ = task.await;
                    }
                    fail(e);
                    return; // guard -> Stopped
                }
            }
        }
        listen_tasks.push(self.start_request_timeout_scanner());
        drop(listen_tasks);
        self.infra_tracker.close();

        // CAS, not a blind store: a shutdown that raced us during Starting has
        // already published ShuttingDown; Running must not resurrect past it
        // (the cancelled token makes every infra task exit immediately).
        if self
            .server_phase
            .compare_exchange(
                PHASE_STARTING,
                PHASE_RUNNING,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            let _ = self.phase_notify.send_replace(PHASE_RUNNING);
        }
        tracing::info!("[TARGET] All protocol servers started, waiting for connections...");

        // Supervise: Stopped only after the tracker has REALLY drained.
        self.infra_tracker.wait().await;
        self.set_phase(PHASE_STOPPED);
        tracing::info!("[STOP] TransportServer infra fully joined");
        drop(guard); // already Stopped: guard is a no-op
    }

    /// [START] Start protocol listener - generic method
    async fn start_protocol_listener(
        &self,
        mut server: Box<dyn crate::Server>,
        protocol_name: String,
    ) -> Result<tokio::task::JoinHandle<()>, TransportError> {
        let server_clone = self.clone();

        let task = self.infra_tracker.spawn(async move {
            let _fail_fast = InfraFailFast {
                cancel: server_clone.infra_cancel.clone(),
                failed: server_clone.infra_failed.clone(),
            };
            tracing::info!("[START] {} listener task started", protocol_name);

            let mut accept_count = 0u64;

            loop {
                tracing::debug!(
                    "[LOOP] {} waiting for connections... (accept count: {})",
                    protocol_name,
                    accept_count
                );

                // Cancellation-driven, not flag-polled: there is no second
                // truth source that a racing serve() could re-arm.
                let accept_result = tokio::select! {
                    _ = server_clone.infra_cancel.cancelled() => {
                        tracing::info!("[STOP] {} listener cancelled", protocol_name);
                        break;
                    }
                    r = server.accept() => r,
                };
                {
                    match accept_result {
                        Ok(mut connection) => {
                            accept_count += 1;
                            tracing::info!(
                                "[SUCCESS] {} accept successful! Connection #{}",
                                protocol_name,
                                accept_count
                            );

                            // Enforce the session cap before allocating a
                            // Transport/actor for this connection. try_acquire on
                            // a shared semaphore is atomic across the concurrent
                            // per-protocol accept loops, so the cap cannot be
                            // raced past the way a len() check could. Rejecting
                            // here keeps an over-capacity flood cheap: accept,
                            // close, move on.
                            let permit =
                                match server_clone.connection_permits.clone().try_acquire_owned() {
                                    Ok(permit) => permit,
                                    Err(_) => {
                                        tracing::warn!(
                                            "[LIMIT] {} connection rejected: at capacity ({})",
                                            protocol_name,
                                            server_clone.max_connections
                                        );
                                        let _ = connection.close().await;
                                        continue;
                                    }
                                };

                            // Get connection info
                            let connection_info = connection.connection_info();
                            let peer_addr = connection_info.peer_addr;

                            tracing::info!(
                                "[CONNECT] New {} connection #{}: {}",
                                protocol_name,
                                accept_count,
                                peer_addr
                            );

                            // Generate new session ID and set to connection
                            let session_id = server_clone.generate_session_id();
                            connection.set_session_id(session_id);
                            tracing::info!(
                                "[ID] Generated session ID for {} connection: {}",
                                protocol_name,
                                session_id
                            );

                            // Add to session management. The session's actor calls
                            // `on_connected` itself, so there is no separate event
                            // to publish here (and no window where a message could
                            // overtake the connect notification).
                            server_clone
                                .add_session_with_permit(connection, permit)
                                .await;
                        }
                        Err(e) => {
                            if server_clone.infra_cancel.is_cancelled() {
                                tracing::info!(
                                    "[STOP] {} listener stopping after accept exit: {:?}",
                                    protocol_name,
                                    e
                                );
                                break;
                            }
                            tracing::warn!(
                                "[WARN] {} accept connection failed, continue listening: {:?}",
                                protocol_name,
                                e
                            );
                            tokio::time::sleep(LISTENER_POLL_INTERVAL).await;
                            continue;
                        }
                    }
                }
            }

            if let Err(e) = server.shutdown().await {
                tracing::warn!("[WARN] {} server shutdown failed: {:?}", protocol_name, e);
            }

            tracing::info!("[STOP] {} server stopped", protocol_name);
        });

        Ok(task)
    }

    fn start_request_timeout_scanner(&self) -> tokio::task::JoinHandle<()> {
        let server_clone = self.clone();
        self.infra_tracker.spawn(async move {
            let _fail_fast = InfraFailFast {
                cancel: server_clone.infra_cancel.clone(),
                failed: server_clone.infra_failed.clone(),
            };
            let tick = server_clone.request_registry.tick_duration();
            tracing::info!(
                "[START] request timeout scanner started (tick={}ms)",
                tick.as_millis()
            );

            loop {
                tokio::select! {
                    _ = server_clone.infra_cancel.cancelled() => break,
                    _ = tokio::time::sleep(tick) => {}
                }
                let timed_out = server_clone.request_registry.scan_timeout_bucket();
                if timed_out > 0 {
                    tracing::warn!(
                        "[TIMEOUT] request timeout scanner marked {} requests as TimedOut",
                        timed_out
                    );
                }
            }

            tracing::info!("[STOP] request timeout scanner stopped");
        })
    }

    /// [INTERNAL] Internal method: extract listen address from protocol configuration
    fn get_protocol_bind_address(
        &self,
        protocol_config: &Box<dyn crate::protocol::adapter::DynServerConfig>,
    ) -> std::net::SocketAddr {
        protocol_config.get_bind_address()
    }

    /// [STOP] Stop accepting new connections (listeners + scanner). Existing
    /// sessions keep running; nothing is awaited. For a verifiable teardown
    /// use [`Self::shutdown`].
    pub async fn stop(&self) {
        tracing::info!("[STOP] Stopping TransportServer accept path");
        self.infra_cancel.cancel();
    }

    /// Graceful, awaitable shutdown with a default 10s deadline.
    pub async fn shutdown(&self) -> ShutdownReport {
        self.shutdown_with_timeout(std::time::Duration::from_secs(10))
            .await
    }

    /// Full supervised shutdown, spending one deadline across every phase:
    ///
    /// 1. Close the admission gate (no new session can form; racers
    ///    self-cancel on their re-check) and stop the listeners/scanner.
    /// 2. Offer every live actor a graceful close (non-blocking), then cancel
    ///    the root token — every session's child token fires, actors leave
    ///    their mailbox waits, and the teardown cascades (actor exit drops the
    ///    mailbox, the pump unparks and reaps, the pipe consumer ends, the
    ///    adapter closes the socket).
    /// 3. Join every session supervisor within the remaining budget. Each
    ///    supervisor owns and joins its actor + pump and releases the permit
    ///    only when both are done — so a joined supervisor PROVES that
    ///    session's tasks ended and its permit returned. A supervisor that
    ///    outlives the budget stays owned in the registry (never detached,
    ///    never cancelled mid-cleanup); a later shutdown joins it.
    /// 4. Wait (bounded) for serve() to finish, which joins the listeners and
    ///    the timeout scanner and frees the TCP/WS/QUIC endpoints.
    ///
    /// `clean` is true only when ALL of it is proven: no session supervisors
    /// left, session map empty, infra finished (or never started), and — when
    /// a cap is configured — every connection permit returned.
    ///
    /// Concurrent and repeated shutdowns serialize on an internal lock and
    /// re-run the idempotent flow; once drained it completes immediately.
    /// A caller whose deadline expires while another shutdown is still
    /// running gets a clean=false snapshot without disturbing the owner.
    ///
    /// Note: handlers' `on_disconnected` is best-effort during shutdown (the
    /// close offer is non-blocking; a full mailbox skips it and cancellation
    /// ends the actor directly).
    pub async fn shutdown_with_timeout(&self, timeout: std::time::Duration) -> ShutdownReport {
        let started = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + timeout;

        // Serialize owners. If another shutdown holds the lock past our
        // budget, report honestly and leave it alone.
        let _owner = match tokio::time::timeout_at(deadline, self.shutdown_lock.lock()).await {
            Ok(guard) => guard,
            Err(_) => {
                return self.report_snapshot(started, 0);
            }
        };

        tracing::info!("[SHUTDOWN] TransportServer shutdown initiated");
        // Phase transition on the CAS truth source. From Idle (never served)
        // we own the whole lifecycle and mark Stopped ourselves; from
        // Starting/Running we flip to ShuttingDown and later wait for serve's
        // guard to publish Stopped; ShuttingDown/Stopped mean an earlier
        // shutdown got here — the flow below is idempotent either way.
        let mut never_served = false;
        loop {
            let cur = self.phase();
            let target = match cur {
                PHASE_IDLE => {
                    never_served = true;
                    PHASE_SHUTTING_DOWN
                }
                PHASE_STARTING | PHASE_RUNNING => PHASE_SHUTTING_DOWN,
                _ => break,
            };
            if self
                .server_phase
                .compare_exchange(
                    cur,
                    target,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
            {
                let _ = self.phase_notify.send_replace(target);
                break;
            }
            never_served = false;
        }
        // Phase 1: gate + stop infra.
        self.admission_open
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // Phase 2: graceful offer, then cancel — both non-blocking, so no
        // session can stall this phase.
        for entry in self.session_handles.keys().unwrap_or_default() {
            if let Some(handle) = self.session_handles.get(&entry) {
                let _ = handle.try_send_event(crate::event::TransportEvent::ConnectionClosed {
                    reason: crate::error::CloseReason::Normal,
                });
            }
        }
        self.root_cancel.cancel();
        // TaskTracker::wait completes only once the tracker is closed AND
        // empty; close() is idempotent, and the admission gate already
        // prevents meaningful new sessions.
        self.session_tracker.close();

        // Phase 3: observe completion through the single owner. The tracker
        // owns every session-supervisor task; wait() is a pure observation —
        // cancel-safe, repeatable, and it can never lose a handle because no
        // handle is ever moved into this future. Tracker emptiness proves all
        // actors and pumps ended and all permits returned.
        let initial_sessions = self.session_tracker.len();
        let _ = tokio::time::timeout_at(deadline, self.session_tracker.wait()).await;
        let sessions_remaining = self.session_tracker.len();
        let sessions_closed = initial_sessions.saturating_sub(sessions_remaining);

        // Phase 4: infra join — wait for serve()'s guard to publish Stopped,
        // which happens only after every listener and the scanner have been
        // joined (freeing the endpoints). A server that never served is
        // finalized to Stopped by us right here.
        if never_served {
            self.set_phase(PHASE_STOPPED);
        }
        let mut phase_rx = self.phase_notify.subscribe();
        let infra_stopped = loop {
            if self.phase() == PHASE_STOPPED {
                break true;
            }
            match tokio::time::timeout_at(deadline, phase_rx.changed()).await {
                Ok(Ok(())) => continue,
                _ => break false,
            }
        };

        // Formal join of the infra supervisor: only taken once finished (a
        // cancellation here could at worst drop a completed handle).
        if infra_stopped {
            let finished = self
                .infra_supervisor
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(false);
            if finished {
                let handle = self
                    .infra_supervisor
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if let Some(handle) = handle {
                    let _ = handle.await;
                }
            }
        }

        // Straggler drain: sessions racing the gate self-cancel on their
        // re-check; observe the tracker for the remaining budget.
        if !self.session_tracker.is_empty() {
            let _ = tokio::time::timeout_at(deadline, self.session_tracker.wait()).await;
        }

        let sessions_remaining = self.session_tracker.len().max(self.transports.len());
        let permits_restored = self.max_connections == usize::MAX
            || self.connection_permits.available_permits() == self.max_connections;
        let clean = sessions_remaining == 0 && infra_stopped && permits_restored;
        let report = ShutdownReport {
            sessions_closed,
            sessions_remaining,
            infra_stopped,
            permits_restored,
            clean,
            elapsed: started.elapsed(),
        };
        tracing::info!(
            "[SHUTDOWN] Complete: closed={} remaining={} infra={} permits={} clean={} elapsed={:?}",
            report.sessions_closed,
            report.sessions_remaining,
            report.infra_stopped,
            report.permits_restored,
            report.clean,
            report.elapsed
        );
        report
    }

    fn report_snapshot(
        &self,
        started: std::time::Instant,
        sessions_closed: usize,
    ) -> ShutdownReport {
        let sessions_remaining = self.session_tracker.len().max(self.transports.len());
        let infra_stopped = self.phase() == PHASE_STOPPED;
        let permits_restored = self.max_connections == usize::MAX
            || self.connection_permits.available_permits() == self.max_connections;
        ShutdownReport {
            sessions_closed,
            sessions_remaining,
            infra_stopped,
            permits_restored,
            clean: false,
            elapsed: started.elapsed(),
        }
    }

    /// Test/introspection accessor for the connection-cap semaphore.
    #[doc(hidden)]
    pub fn available_permits(&self) -> usize {
        self.connection_permits.available_permits()
    }

    /// Test/introspection accessor: live session-supervisor tasks in the
    /// tracker (the single owner). 0 when every session runtime has ended.
    #[doc(hidden)]
    pub fn live_session_tasks(&self) -> usize {
        self.session_tracker.len()
    }
}

/// What a [`TransportServer::shutdown`] accomplished. See the LIMITATION on
/// [`TransportServer::shutdown_with_timeout`] for what `clean` does and does
/// not yet prove.
#[derive(Debug, Clone)]
pub struct ShutdownReport {
    /// Session supervisors joined by this shutdown (each join proves that
    /// session's actor + pump ended and its permit returned).
    pub sessions_closed: usize,
    /// Sessions still draining when the deadline hit (0 on a clean shutdown).
    pub sessions_remaining: usize,
    /// serve() — listeners and the timeout scanner — has returned (or was
    /// never started), so the protocol endpoints are freed.
    pub infra_stopped: bool,
    /// Every connection permit is back (always true when uncapped).
    pub permits_restored: bool,
    /// True iff everything is proven: supervisors joined, maps empty, infra
    /// finished, permits restored.
    pub clean: bool,
    /// Wall time the shutdown took.
    pub elapsed: std::time::Duration,
}

impl Clone for TransportServer {
    fn clone(&self) -> Self {
        // Clone protocol configuration - using clone_server_dyn()
        let mut cloned_configs = std::collections::HashMap::new();
        for (name, config) in &self.protocol_configs {
            cloned_configs.insert(name.clone(), config.clone_server_dyn());
        }

        Self {
            config: self.config.clone(),
            context: self.context.clone(),
            transports: self.transports.clone(),
            session_id_generator: self.session_id_generator.clone(),
            stats: self.stats.clone(),
            protocol_configs: cloned_configs,
            state_manager: self.state_manager.clone(),
            request_registry: self.request_registry.clone(),
            session_handles: self.session_handles.clone(),
            session_handler: self.session_handler.clone(),
            actor_buffer_size: self.actor_buffer_size,
            frame_policy: self.frame_policy,
            connection_permits: self.connection_permits.clone(),
            max_connections: self.max_connections,
            root_cancel: self.root_cancel.clone(),
            infra_cancel: self.infra_cancel.clone(),
            session_tracker: self.session_tracker.clone(),
            session_cancels: self.session_cancels.clone(),
            infra_tracker: self.infra_tracker.clone(),
            infra_supervisor: self.infra_supervisor.clone(),
            infra_failed: self.infra_failed.clone(),
            serve_error: self.serve_error.clone(),
            admission_open: self.admission_open.clone(),
            server_phase: self.server_phase.clone(),
            phase_notify: self.phase_notify.clone(),
            shutdown_lock: self.shutdown_lock.clone(),
        }
    }
}

impl std::fmt::Debug for TransportServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportServer")
            .field("session_count", &self.transports.len())
            .field("config", &self.config)
            .finish()
    }
}

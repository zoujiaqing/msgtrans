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
    is_running: Arc<std::sync::atomic::AtomicBool>,
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

        Ok(Self {
            config,
            context: ctx,
            transports: Arc::new(LockFreeHashMap::new()),
            session_handles: Arc::new(LockFreeHashMap::new()),
            session_id_generator: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            stats: Arc::new(LockFreeHashMap::new()),
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        })
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
        let event_receiver_opt = connection.event_stream();

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
        let actor = actor
            .with_inbound_registry(Some(self.request_registry.clone()))
            .with_connection_permit(Some(permit));
        let actor_handle = match self.session_handles.insert(session_id, handle.clone()) {
            Ok(_) => {
                tokio::spawn(actor.run());
                Some(handle)
            }
            Err(e) => {
                tracing::error!(
                    "[ERROR] Failed to register session actor handle for {}: {:?}",
                    session_id,
                    e
                );
                None
            }
        };

        // [LOOP] Pump the connection's transport events into this session's actor.
        // Bounded-backbone adapters hand over a single-consumer pipe (real
        // backpressure; ends with exactly one ConnectionClosed). Legacy
        // adapters still expose the broadcast facade until their migration.
        if let Some(mut pipe) = event_pipe_opt {
            let server_clone = self.clone();
            tokio::spawn(async move {
                tracing::info!(
                    "[LISTENER] TransportServer pump started for session {} (bounded pipe)",
                    session_id
                );
                while let Some(transport_event) = pipe.next().await {
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
            });
        } else if let Some(mut event_receiver) = event_receiver_opt {
            let server_clone = self.clone();
            tokio::spawn(async move {
                tracing::info!("[LISTENER] TransportServer starting event consumption loop for session {} (pre-subscribed)", session_id);
                while let Ok(transport_event) = event_receiver.recv().await {
                    tracing::trace!(
                        "[EVENT] TransportServer received event from session {}: {:?}",
                        session_id,
                        transport_event
                    );
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
                tracing::info!(
                    "[END] TransportServer event consumption loop ended for session {}",
                    session_id
                );
            });
        } else {
            tracing::warn!(
                "[WARN] Session {} unable to get event stream before connection setup",
                session_id
            );
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
        self.is_running
            .store(true, std::sync::atomic::Ordering::SeqCst);

        if self.protocol_configs.is_empty() {
            tracing::warn!("[WARN] No protocols configured, server cannot start listening");
            self.is_running
                .store(false, std::sync::atomic::Ordering::SeqCst);
            return Err(TransportError::config_error(
                "protocols",
                "No protocols configured",
            ));
        }

        tracing::info!(
            "[START] Starting {} protocol servers",
            self.protocol_configs.len()
        );

        // Create vector of listen tasks
        let mut listen_tasks = Vec::new();

        // Start server for each protocol configuration
        for (protocol_name, protocol_config) in &self.protocol_configs {
            tracing::info!("[CONFIG] Processing protocol: {}", protocol_name);

            let address = self.get_protocol_bind_address(protocol_config);
            tracing::info!(
                "[BIND] Protocol {} bind address: {}",
                protocol_name,
                address
            );

            match protocol_config.build_server_dyn().await {
                Ok(server) => {
                    match self
                        .start_protocol_listener(server, protocol_name.clone())
                        .await
                    {
                        Ok(listener_task) => {
                            listen_tasks.push(listener_task);
                            tracing::info!(
                                "[SUCCESS] {} server started successfully: {}",
                                protocol_name,
                                address
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "[ERROR] {} listener task creation failed: {:?}",
                                protocol_name,
                                e
                            );
                            self.is_running
                                .store(false, std::sync::atomic::Ordering::SeqCst);
                            for task in listen_tasks {
                                task.abort();
                            }
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("[ERROR] {} server build failed: {:?}", protocol_name, e);
                    self.is_running
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    for task in listen_tasks {
                        task.abort();
                    }
                    return Err(e);
                }
            }
        }

        listen_tasks.push(self.start_request_timeout_scanner());

        tracing::info!("[TARGET] All protocol servers started, waiting for connections...");

        // Wait for all listen tasks to complete
        for (index, task) in listen_tasks.into_iter().enumerate() {
            tracing::info!("[WAIT] Waiting for task {} to complete...", index + 1);
            if let Err(e) = task.await {
                tracing::error!("[ERROR] Task {} was cancelled: {:?}", index + 1, e);
                return Err(TransportError::config_error(
                    "server",
                    "Listener task cancelled",
                ));
            }
        }

        tracing::info!("[STOP] TransportServer stopped");
        Ok(())
    }

    /// [START] Start protocol listener - generic method
    async fn start_protocol_listener(
        &self,
        mut server: Box<dyn crate::Server>,
        protocol_name: String,
    ) -> Result<tokio::task::JoinHandle<()>, TransportError> {
        let server_clone = self.clone();

        let task = tokio::spawn(async move {
            tracing::info!("[START] {} listener task started", protocol_name);

            let mut accept_count = 0u64;

            loop {
                if !server_clone
                    .is_running
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    tracing::info!("[STOP] {} listener received stop signal", protocol_name);
                    break;
                }

                tracing::debug!(
                    "[LOOP] {} waiting for connections... (accept count: {})",
                    protocol_name,
                    accept_count
                );

                match tokio::time::timeout(LISTENER_POLL_INTERVAL, server.accept()).await {
                    Err(_) => {
                        // Poll timeout, loop again and check stop flag.
                        continue;
                    }
                    Ok(accept_result) => match accept_result {
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
                            if !server_clone
                                .is_running
                                .load(std::sync::atomic::Ordering::SeqCst)
                            {
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
                    },
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
        tokio::spawn(async move {
            let tick = server_clone.request_registry.tick_duration();
            tracing::info!(
                "[START] request timeout scanner started (tick={}ms)",
                tick.as_millis()
            );

            loop {
                if !server_clone
                    .is_running
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    break;
                }

                tokio::time::sleep(tick).await;
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

    /// [STOP] Stop server
    pub async fn stop(&self) {
        tracing::info!("[STOP] Stopping TransportServer");
        self.is_running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
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
            is_running: self.is_running.clone(),
            protocol_configs: cloned_configs,
            state_manager: self.state_manager.clone(),
            request_registry: self.request_registry.clone(),
            session_handles: self.session_handles.clone(),
            session_handler: self.session_handler.clone(),
            actor_buffer_size: self.actor_buffer_size,
            frame_policy: self.frame_policy,
            connection_permits: self.connection_permits.clone(),
            max_connections: self.max_connections,
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

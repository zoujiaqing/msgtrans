use crate::{
    connection::Connection,
    event::TransportEvent,
    protocol::ProtocolRegistry,
    transport::{
        config::TransportConfig, connection_state::ConnectionStateManager,
        context::TransportContext, memory_pool::OptimizedMemoryPool,
    },
    Packet, SessionId, TransportError,
};
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Single connection transport abstraction — one instance per socket.
///
/// v1.3: Constructor is now synchronous. Heavy resources come from `TransportContext`
/// which is created once by the builder and shared across all Transport instances.
pub struct Transport {
    config: TransportConfig,
    protocol_registry: Arc<ProtocolRegistry>,
    memory_pool: Arc<OptimizedMemoryPool>,
    connection: Arc<Mutex<Option<Box<dyn Connection>>>>,
    session_id: Arc<Mutex<Option<SessionId>>>,
    state_manager: ConnectionStateManager,
    /// Client-facing event queue: bounded, single consumer (the client's
    /// forwarding task). Replaces the broadcast hop, whose one Lagged killed
    /// the forwarding task and silently ended all client event delivery.
    client_events_tx: mpsc::Sender<TransportEvent>,
    client_events_rx: Arc<Mutex<Option<mpsc::Receiver<TransportEvent>>>>,
    request_registry: Arc<crate::transport::request_registry::RequestRegistry>,
}
/// Fallback lifecycle deadline for waiter-based requests. The real timeout is
/// enforced by the caller (tokio::time::timeout); this only bounds the entry if
/// the caller forgets to remove it.
const REQUEST_WAITER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
impl Transport {
    /// Create Transport from a shared context (synchronous — no global singletons).
    pub fn with_context(config: TransportConfig, ctx: &TransportContext) -> Self {
        let (client_events_tx, client_events_rx) = mpsc::channel(8192);
        Self {
            config,
            protocol_registry: ctx.protocol_registry.clone(),
            memory_pool: ctx.memory_pool.clone(),
            connection: Arc::new(Mutex::new(None)),
            session_id: Arc::new(Mutex::new(None)),
            state_manager: ConnectionStateManager::new(),
            client_events_tx,
            client_events_rx: Arc::new(Mutex::new(Some(client_events_rx))),
            request_registry: Arc::new(crate::transport::request_registry::RequestRegistry::new()),
        }
    }

    /// [TARGET] Core method: establish connection with protocol configuration
    /// This is the connection method needed by TransportClient
    pub async fn connect_with_config<T>(
        self: &Arc<Self>,
        config: T,
    ) -> Result<SessionId, TransportError>
    where
        T: crate::protocol::client_config::ConnectableConfig,
    {
        // Directly use the current Transport Arc instance
        config.connect(Arc::clone(self)).await
    }

    /// Send data packet through the underlying connection (single-lock hot path).
    pub async fn send(&self, packet: Packet) -> Result<(), TransportError> {
        let mut guard = self.connection.lock().await;
        match guard.as_mut() {
            Some(conn) => conn.send(packet).await,
            None => Err(TransportError::connection_error("Not connected", false)),
        }
    }

    /// Apply a frame decode policy to the underlying connection (if connected).
    pub(crate) async fn set_frame_policy(&self, policy: crate::packet::FramePolicy) {
        if let Some(conn) = self.connection.lock().await.as_ref() {
            conn.set_frame_policy(policy);
        }
    }

    /// [TARGET] Core method: disconnect connection (graceful shutdown)
    pub async fn disconnect(&self) -> Result<(), TransportError> {
        if let Some(session_id) = self.current_session_id().await {
            self.close_session(session_id).await
        } else {
            Err(TransportError::connection_error("Not connected", false))
        }
    }

    /// [TARGET] Unified close method: graceful session shutdown
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
        let failed_pending = self.request_registry.abort_all();
        if failed_pending > 0 {
            tracing::debug!(
                "[REQUEST] Failed {} pending requests during session {} shutdown",
                failed_pending,
                session_id
            );
        }

        // 2. Execute actual close logic (underlying adapter will automatically send close event)
        self.do_close_session(session_id).await?;

        // 3. Mark as closed
        self.state_manager.mark_closed(session_id).await;

        if self.session_id.lock().await.as_ref() == Some(&session_id) {
            *self.session_id.lock().await = None;
            *self.connection.lock().await = None;
        }

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
        let failed_pending = self.request_registry.abort_all();
        if failed_pending > 0 {
            tracing::debug!(
                "[REQUEST] Failed {} pending requests during session {} force close",
                failed_pending,
                session_id
            );
        }

        if let Some(conn) = self.connection.lock().await.as_mut() {
            let _ = conn.close().await;
        }

        self.state_manager.mark_closed(session_id).await;

        if self.session_id.lock().await.as_ref() == Some(&session_id) {
            *self.session_id.lock().await = None;
            *self.connection.lock().await = None;
        }

        tracing::info!("[SUCCESS] Session {} force close complete", session_id);
        Ok(())
    }

    async fn do_close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        let mut guard = self.connection.lock().await;
        if let Some(conn) = guard.as_mut() {
            match tokio::time::timeout(
                self.config.graceful_timeout,
                self.try_graceful_close(&mut **conn),
            )
            .await
            {
                Ok(Ok(_)) => {
                    tracing::debug!("[SUCCESS] Session {} graceful close successful", session_id);
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        "[WARN] Session {} graceful close failed, executing force close: {:?}",
                        session_id,
                        e
                    );
                    let _ = conn.close().await;
                }
                Err(_) => {
                    tracing::warn!(
                        "[WARN] Session {} graceful close timeout, executing force close",
                        session_id
                    );
                    let _ = conn.close().await;
                }
            }
        }

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

    /// Check if messages should be ignored for this session
    pub async fn should_ignore_messages(&self, session_id: SessionId) -> bool {
        self.state_manager.should_ignore_messages(session_id).await
    }

    /// [TARGET] Core method: check connection status
    pub async fn is_connected(&self) -> bool {
        self.session_id.lock().await.is_some()
    }

    /// [TARGET] Core method: get current session ID
    pub async fn current_session_id(&self) -> Option<SessionId> {
        self.session_id.lock().await.as_ref().cloned()
    }

    /// Set connection and start internal event consumer (used by TransportClient).
    pub async fn set_connection(
        self: &Arc<Self>,
        mut connection: Box<dyn Connection>,
        session_id: SessionId,
    ) {
        connection.set_session_id(session_id);
        let event_pipe_opt = connection.take_event_pipe();

        *self.connection.lock().await = Some(connection);
        *self.session_id.lock().await = Some(session_id);
        self.state_manager.add_connection(session_id);
        // Open request tracking for this session; the registry refuses
        // registrations for sessions that were never opened or already closed.
        self.request_registry.open_session(session_id);
        tracing::debug!("[SUCCESS] Transport connection set: {}", session_id);

        if let Some(mut pipe) = event_pipe_opt {
            // Bounded backbone: single-consumer queue with backpressure; the
            // pipe ends with exactly one ConnectionClosed.
            let this = Arc::clone(self);
            tokio::spawn(async move {
                tracing::debug!(
                    "[LISTEN] Transport event consumer started (pipe, session: {})",
                    session_id
                );
                while let Some(event) = pipe.next().await {
                    this.on_event(event).await;
                }
                let failed_pending = this.request_registry.abort_all();
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
        *self.connection.lock().await = Some(connection);
        *self.session_id.lock().await = Some(session_id);
        self.state_manager.add_connection(session_id);
        tracing::debug!(
            "[SUCCESS] Transport connection set (no consumer): {}",
            session_id
        );
    }

    /// Get protocol registry
    pub fn protocol_registry(&self) -> &ProtocolRegistry {
        &self.protocol_registry
    }

    /// Get configuration
    pub fn config(&self) -> &TransportConfig {
        &self.config
    }

    pub fn memory_pool_stats(&self) -> crate::transport::memory_pool::OptimizedMemoryStatsSnapshot {
        self.memory_pool.get_stats()
    }

    /// Take the client event queue (single consumer, once). The queue spans
    /// reconnects: new connections' pipe consumers feed the same sender.
    pub async fn get_event_stream(
        &self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::event::TransportEvent>> {
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
        let rx = match self.request_registry.try_register_waiter(
            client_message_id,
            session_id,
            packet.header.biz_type,
            REQUEST_WAITER_TIMEOUT,
        ) {
            Ok(rx) => rx,
            Err(_) => {
                return Err(TransportError::connection_error(
                    "Duplicate in-flight request id",
                    false,
                ))
            }
        };

        if let Err(e) = self.send(packet).await {
            self.request_registry
                .abort_waiter(session_id, client_message_id);
            return Err(e);
        }
        let timeout_duration = std::time::Duration::from_secs(10);
        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(TransportError::connection_error("Connection closed", true)),
            Err(_) => {
                self.request_registry
                    .abort_waiter(session_id, client_message_id);
                Err(TransportError::timeout_error("request", timeout_duration))
            }
        }
    }

    /// [TARGET] Decompress and unpack Packet payload, hiding protocol complexity
    fn decode_payload(&self, packet: &Packet) -> Result<Bytes, TransportError> {
        // [FIX] If packet is compressed, decompress it
        if packet.header.compression != crate::packet::CompressionType::None {
            let mut packet_copy = packet.clone();
            match packet_copy.decompress_payload() {
                Ok(_) => Ok(packet_copy.payload),
                Err(e) => {
                    tracing::warn!("[WARN] Failed to decompress packet: {}", e);
                    Err(TransportError::protocol_error(
                        "packet",
                        format!("Failed to decompress packet: {}", e),
                    ))
                }
            }
        } else {
            Ok(packet.payload.clone())
        }
    }

    /// [TARGET] Unified event handling entry point - complete unpacking and send user-friendly events at this layer
    pub async fn on_event(&self, event: crate::event::TransportEvent) {
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
                        tracing::info!(
                            "[RECV] Processing response packet: ID={}, type={:?}, biz_type={}",
                            id,
                            packet.header.packet_type,
                            packet.header.biz_type
                        );
                        let session_id = self.current_session_id().await;
                        let completed =
                            self.request_registry
                                .complete_waiter(session_id, id, packet.clone());
                        tracing::info!(
                            "[PROC] Response packet processing result: ID={}, completed={}",
                            id,
                            completed
                        );
                        if !completed {
                            tracing::warn!("[WARN] Response packet ID={} not found in request tracker, may be timeout or duplicate", id);
                            // Forward unmatched responses so higher layers can handle them.
                            self.forward_client_event(
                                crate::event::TransportEvent::MessageReceived(packet),
                            )
                            .await;
                        }
                    }

                    crate::packet::PacketType::Request => {
                        let id = packet.header.message_id;
                        tracing::debug!("[PROC] Received request packet, creating unified TransportContext: ID={}, type={:?}", id, packet.header.packet_type);

                        // [TARGET] Send MessageReceived event directly, let ClientEvent handle Request logic during conversion
                        tracing::debug!(
                            "[SEND] Sending unified MessageReceived event (Request): ID={}",
                            id
                        );
                        self.forward_client_event(crate::event::TransportEvent::MessageReceived(
                            packet,
                        ))
                        .await;
                    }

                    crate::packet::PacketType::OneWay => {
                        tracing::debug!(
                            "[RECV] Processing one-way message packet: ID={}, type={:?}",
                            packet.header.message_id,
                            packet.header.packet_type
                        );

                        // [TARGET] Unpack data
                        match self.decode_payload(&packet) {
                            Ok(data) => {
                                let session_id = self.session_id.lock().await.as_ref().cloned();

                                // [TARGET] Create user-friendly Message
                                let _message = crate::event::Message {
                                    peer: session_id,
                                    data,
                                    message_id: packet.header.message_id,
                                };

                                // [TARGET] Send user-friendly message event (maintain backward compatibility)
                                self.forward_client_event(
                                    crate::event::TransportEvent::MessageReceived(packet),
                                )
                                .await;
                            }
                            Err(e) => {
                                tracing::error!("[ERROR] Failed to unpack message data: {}", e);
                                self.forward_client_event(
                                    crate::event::TransportEvent::TransportError { error: e },
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            crate::event::TransportEvent::ConnectionClosed { reason } => {
                let failed_pending = self.request_registry.abort_all();
                if failed_pending > 0 {
                    tracing::debug!(
                        "[REQUEST] Failed {} pending requests after connection closed: {:?}",
                        failed_pending,
                        reason
                    );
                }
                self.forward_client_event(crate::event::TransportEvent::ConnectionClosed {
                    reason,
                })
                .await;
            }
            // Forward other events directly
            _ => {
                tracing::trace!("[SEND] Forwarding other event: {:?}", event);
                self.forward_client_event(event).await;
            }
        }
    }

    /// Forward an event to the client's bounded queue. Backpressures the pipe
    /// consumer (and through it the adapter and socket); if the client dropped
    /// its receiver, events are discarded — there is no consumer to lose them.
    async fn forward_client_event(&self, event: TransportEvent) {
        let _ = self.client_events_tx.send(event).await;
    }

    /// Send data packet and wait for response (with options)
    pub async fn request_with_options(
        &self,
        data: Bytes,
        options: super::TransportOptions,
    ) -> Result<Bytes, TransportError> {
        // Use user-provided message_id or generate new one
        let message_id = options
            .message_id
            .unwrap_or_else(|| self.request_registry.next_message_id());

        // Create request packet
        let mut packet = crate::packet::Packet {
            header: crate::packet::FixedHeader {
                version: 1,
                compression: options
                    .compression
                    .unwrap_or(crate::packet::CompressionType::None),
                packet_type: crate::packet::PacketType::Request,
                biz_type: options.biz_type.unwrap_or(0),
                message_id,
                ext_header_len: options.ext_header.as_ref().map_or(0, |h| h.len() as u16),
                payload_len: data.len() as u32,
                reserved: crate::packet::ReservedFlags::new(),
            },
            ext_header: options.ext_header.unwrap_or_default().to_vec(),
            payload: data.clone(),
        };

        // [FIX] If compression is needed, compress the packet
        if options.compression.is_some()
            && options.compression != Some(crate::packet::CompressionType::None)
        {
            if let Err(e) = packet.compress_payload() {
                tracing::warn!("[WARN] Failed to compress packet: {}, using raw data", e);
            }
        }

        // Register request tracking
        let session_id = self.current_session_id().await;
        let rx = match self.request_registry.try_register_waiter(
            message_id,
            session_id,
            packet.header.biz_type,
            REQUEST_WAITER_TIMEOUT,
        ) {
            Ok(rx) => rx,
            Err(_) => {
                return Err(TransportError::connection_error(
                    "Duplicate in-flight request id",
                    false,
                ))
            }
        };

        tracing::info!(
            "[SEND] Sending request: message_id={}, biz_type={}, timeout={:?}",
            message_id,
            packet.header.biz_type,
            options.timeout
        );

        // Send packet
        if let Err(e) = self.send(packet).await {
            self.request_registry.abort_waiter(session_id, message_id);
            return Err(e);
        }

        tracing::info!(
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
                tracing::info!(
                    "[SUCCESS] Received response: message_id={}, biz_type={}, payload_len={}",
                    message_id,
                    resp.header.biz_type,
                    resp.payload.len()
                );
                // [FIX] Decompress response data
                self.decode_payload(&resp).map(Bytes::from)
            }
            Ok(Err(_)) => {
                tracing::warn!("[WARN] Response channel closed: message_id={}", message_id);
                Err(TransportError::connection_error("Connection closed", true))
            }
            Err(_) => {
                self.request_registry.abort_waiter(session_id, message_id);
                tracing::warn!(
                    "[WARN] Request timeout: message_id={}, timeout={:?}",
                    message_id,
                    timeout_duration
                );
                Err(TransportError::timeout_error("request", timeout_duration))
            }
        }
    }

    pub(crate) fn next_message_id(&self) -> u32 {
        self.request_registry.next_message_id()
    }

    /// Send one-way message (with options)
    pub async fn send_with_options(
        &self,
        data: Bytes,
        options: super::TransportOptions,
    ) -> Result<(), TransportError> {
        // Use user-provided message_id or generate new one
        let message_id = options
            .message_id
            .unwrap_or_else(|| self.request_registry.next_message_id());

        // Create one-way message packet
        let mut packet = crate::packet::Packet {
            header: crate::packet::FixedHeader {
                version: 1,
                compression: options
                    .compression
                    .unwrap_or(crate::packet::CompressionType::None),
                packet_type: crate::packet::PacketType::OneWay,
                biz_type: options.biz_type.unwrap_or(0),
                message_id,
                ext_header_len: options.ext_header.as_ref().map_or(0, |h| h.len() as u16),
                payload_len: data.len() as u32,
                reserved: crate::packet::ReservedFlags::new(),
            },
            ext_header: options.ext_header.unwrap_or_default().to_vec(),
            payload: data.clone(),
        };

        // [FIX] If compression is needed, compress the packet
        if options.compression.is_some()
            && options.compression != Some(crate::packet::CompressionType::None)
        {
            if let Err(e) = packet.compress_payload() {
                tracing::warn!("[WARN] Failed to compress packet: {}, using raw data", e);
            }
        }

        // Send packet
        self.send(packet).await?;
        Ok(())
    }
}

impl Clone for Transport {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            protocol_registry: self.protocol_registry.clone(),
            memory_pool: self.memory_pool.clone(),
            connection: self.connection.clone(),
            session_id: self.session_id.clone(),
            state_manager: self.state_manager.clone(),
            client_events_tx: self.client_events_tx.clone(),
            client_events_rx: self.client_events_rx.clone(),
            request_registry: self.request_registry.clone(),
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

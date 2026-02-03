use crate::{
    adapters::create_standard_registry,
    connection::Connection,
    event::TransportEvent,
    protocol::ProtocolRegistry,
    transport::{
        config::TransportConfig, connection_state::ConnectionStateManager,
        memory_pool::OptimizedMemoryPool, pool::ConnectionPool,
    },
    Packet, SessionId, TransportError,
};
use bytes::Bytes;
use dashmap::DashMap;
/// [TARGET] Single connection transport abstraction - each instance manages one socket connection
///
/// This is the correct Transport abstraction:
/// - Each Transport corresponds to one socket connection
/// - Provides send() method to send data directly to socket
/// - Protocol-agnostic design
/// - Used by TransportClient (single connection) and TransportServer (multi-connection management)
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use tokio::sync::{broadcast, oneshot, Mutex};

/// [TARGET] Single connection transport abstraction - properly aligned with architecture design
pub struct Transport {
    /// Configuration
    config: TransportConfig,
    /// Protocol registry
    protocol_registry: Arc<ProtocolRegistry>,
    /// Optimized connection pool
    connection_pool: Arc<ConnectionPool>,
    /// Optimized memory pool
    memory_pool: Arc<OptimizedMemoryPool>,
    /// [TARGET] Single connection adapter - represents this socket connection
    connection_adapter: Arc<Mutex<Option<Arc<Mutex<Box<dyn Connection>>>>>>,
    /// Current connection session ID
    session_id: Arc<Mutex<Option<SessionId>>>,
    /// Connection state manager
    state_manager: ConnectionStateManager,
    event_sender: broadcast::Sender<TransportEvent>,
    request_tracker: Arc<RequestTracker>,
}

pub struct RequestTracker {
    pending: DashMap<u32, oneshot::Sender<Packet>>,
    next_id: AtomicU32,
}

impl RequestTracker {
    pub fn new() -> Self {
        Self {
            pending: DashMap::new(),
            next_id: AtomicU32::new(1),
        }
    }

    /// Create RequestTracker with custom starting ID
    pub fn new_with_start_id(start_id: u32) -> Self {
        Self {
            pending: DashMap::new(),
            next_id: AtomicU32::new(start_id),
        }
    }
    pub fn register(&self) -> (u32, oneshot::Receiver<Packet>) {
        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending.insert(id, tx);
        (id, rx)
    }

    /// [FIX] Register request tracking with specified ID
    pub fn register_with_id(&self, id: u32) -> (u32, oneshot::Receiver<Packet>) {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        (id, rx)
    }
    pub fn complete(&self, id: u32, packet: Packet) -> bool {
        if let Some((_, tx)) = self.pending.remove(&id) {
            let _ = tx.send(packet);
            true
        } else {
            false
        }
    }
    pub fn clear(&self) {
        self.pending.clear();
    }
}

impl Transport {
    /// Create new single connection transport
    pub async fn new(config: TransportConfig) -> Result<Self, TransportError> {
        tracing::info!("[PERF] Creating Transport");

        // Create protocol registry
        let protocol_registry = Arc::new(create_standard_registry().await?);

        // Create connection pool and memory pool (simplified version)
        let connection_pool = Arc::new(ConnectionPool::new(2, 10).initialize_pool().await?);

        let memory_pool = Arc::new(OptimizedMemoryPool::new());

        let (event_sender, _) = broadcast::channel(8192);

        Ok(Self {
            config,
            protocol_registry,
            connection_pool,
            memory_pool,
            connection_adapter: Arc::new(Mutex::new(None)),
            session_id: Arc::new(Mutex::new(None)),
            state_manager: ConnectionStateManager::new(),
            event_sender,
            request_tracker: Arc::new(RequestTracker::new()),
        })
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

    /// [TARGET] Core method: send data packet to current connection
    pub async fn send(&self, packet: Packet) -> Result<(), TransportError> {
        if let Some(session_id) = self.session_id.lock().await.as_ref() {
            // [FIX] Implement real sending logic
            if let Some(connection_adapter) = &self.connection_adapter.lock().await.as_ref() {
                tracing::debug!("[SEND] Transport sending packet (session: {})", session_id);

                // Get lock and directly call the generic send method
                let mut connection = connection_adapter.lock().await;

                tracing::debug!("[SEND] Using generic connection to send packet");

                // Call generic Connection::send method
                match connection.send(packet).await {
                    Ok(_) => {
                        tracing::debug!("[SEND] Packet sent successfully");
                        Ok(())
                    }
                    Err(e) => {
                        tracing::error!("[SEND] Packet send failed: {:?}", e);
                        Err(e)
                    }
                }
            } else {
                tracing::error!("[ERROR] No connection adapter available");
                Err(TransportError::connection_error(
                    "No connection adapter available",
                    false,
                ))
            }
        } else {
            Err(TransportError::connection_error("Not connected", false))
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

        // 2. Execute actual close logic (underlying adapter will automatically send close event)
        self.do_close_session(session_id).await?;

        // 3. Mark as closed
        self.state_manager.mark_closed(session_id).await;

        // 4. Clean up local state
        if self.session_id.lock().await.as_ref() == Some(&session_id) {
            *self.session_id.lock().await = None;
            *self.connection_adapter.lock().await = None;
        }

        tracing::info!("[SUCCESS] Session {} shutdown complete", session_id);
        Ok(())
    }

    /// [TARGET] Force close session
    pub async fn force_close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        // 1. Check if we can start closing
        if !self.state_manager.try_start_closing(session_id).await {
            tracing::debug!(
                "Session {} already closing or closed, skipping force close",
                session_id
            );
            return Ok(());
        }

        tracing::info!("[CONN] Force closing session: {}", session_id);

        // 2. Immediately force close, don't wait
        if let Some(connection_adapter) = &self.connection_adapter.lock().await.as_ref() {
            let mut conn = connection_adapter.lock().await;
            let _ = conn.close().await; // Ignore errors, close directly
        }

        // 3. Mark as closed
        self.state_manager.mark_closed(session_id).await;

        // 4. Clean up local state
        if self.session_id.lock().await.as_ref() == Some(&session_id) {
            *self.session_id.lock().await = None;
            *self.connection_adapter.lock().await = None;
        }

        tracing::info!("[SUCCESS] Session {} force close complete", session_id);
        Ok(())
    }

    /// Internal method: perform actual session close
    async fn do_close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        if let Some(connection_adapter) = &self.connection_adapter.lock().await.as_ref() {
            let mut conn = connection_adapter.lock().await;

            // Try graceful close
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
                    let _ = conn.close().await; // Ignore errors, close directly
                }
                Err(_) => {
                    tracing::warn!(
                        "[WARN] Session {} graceful close timeout, executing force close",
                        session_id
                    );
                    let _ = conn.close().await; // Ignore errors, close directly
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

    /// Set connection adapter and session ID (internal use)
    pub async fn set_connection(
        self: &Arc<Self>,
        mut connection: Box<dyn Connection>,
        session_id: SessionId,
    ) {
        // [FIX] Set connection session_id
        connection.set_session_id(session_id);

        // [FIX] Subscribe to event stream BEFORE setting connection to ensure we don't miss events
        // This ensures that when adapter's event loop starts sending events, there's already a subscriber
        let event_receiver_opt = connection.event_stream();

        *self.connection_adapter.lock().await = Some(Arc::new(Mutex::new(connection)));
        *self.session_id.lock().await = Some(session_id);
        self.state_manager.add_connection(session_id);
        tracing::debug!(
            "[SUCCESS] Transport connection setup complete: {}",
            session_id
        );

        // Start event consumer loop, ensure all TransportEvent are handled uniformly in on_event
        if let Some(mut event_receiver) = event_receiver_opt {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                tracing::debug!(
                    "[LISTEN] Transport event consumer loop started (session: {})",
                    session_id
                );
                while let Ok(event) = event_receiver.recv().await {
                    tracing::trace!("[RECV] Transport received event: {:?}", event);
                    this.on_event(event).await;
                }
                tracing::debug!(
                    "[LISTEN] Transport event consumer loop ended (session: {})",
                    session_id
                );
            });
        } else {
            tracing::warn!(
                "[WARN] Connection doesn't support event stream (session: {})",
                session_id
            );
        }
    }

    /// Get protocol registry
    pub fn protocol_registry(&self) -> &ProtocolRegistry {
        &self.protocol_registry
    }

    /// Get configuration
    pub fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Get connection pool statistics
    pub fn connection_pool_stats(&self) -> crate::transport::pool::OptimizedPoolStatsSnapshot {
        self.connection_pool.get_performance_stats()
    }

    /// Get memory pool statistics
    pub fn memory_pool_stats(&self) -> crate::transport::memory_pool::OptimizedMemoryStatsSnapshot {
        self.memory_pool.get_stats()
    }

    /// Get connection adapter (for message reception)
    pub async fn connection_adapter(&self) -> Option<Arc<Mutex<Box<dyn Connection>>>> {
        self.connection_adapter.lock().await.as_ref().cloned()
    }

    /// Get connection event stream (if supported)
    ///
    /// [FIX] Return Transport's high-level event stream, not Connection's raw event stream
    /// This way we can receive RequestReceived and other events processed by Transport
    pub async fn get_event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        // Check if there's a connection
        if self.connection_adapter.lock().await.is_some() {
            // Return Transport's event sender subscription
            Some(self.event_sender.subscribe())
        } else {
            None
        }
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
        let (_, rx) = self.request_tracker.register_with_id(client_message_id);

        self.send(packet).await?;
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(TransportError::connection_error("Connection closed", false)),
            Err(_) => Err(TransportError::connection_error("Request timeout", false)),
        }
    }

    /// [TARGET] Decompress and unpack Packet payload, hiding protocol complexity
    fn decode_payload(&self, packet: &Packet) -> Result<Vec<u8>, TransportError> {
        // [FIX] If packet is compressed, decompress it
        if packet.header.compression != crate::packet::CompressionType::None {
            let mut packet_copy = packet.clone();
            match packet_copy.decompress_payload() {
                Ok(_) => Ok(packet_copy.payload),
                Err(e) => {
                    tracing::warn!("[WARN] Failed to decompress packet: {}, using raw data", e);
                    Ok(packet.payload.clone())
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
                        let completed = self.request_tracker.complete(id, packet);
                        tracing::info!(
                            "[PROC] Response packet processing result: ID={}, completed={}",
                            id,
                            completed
                        );
                        if !completed {
                            tracing::warn!("[WARN] Response packet ID={} not found in request tracker, may be timeout or duplicate", id);
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
                        let _ = self
                            .event_sender
                            .send(crate::event::TransportEvent::MessageReceived(packet));
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
                                let _ = self
                                    .event_sender
                                    .send(crate::event::TransportEvent::MessageReceived(packet));
                            }
                            Err(e) => {
                                tracing::error!("[ERROR] Failed to unpack message data: {}", e);
                                let _ = self.event_sender.send(
                                    crate::event::TransportEvent::TransportError { error: e },
                                );
                            }
                        }
                    }
                }
            }
            // Forward other events directly
            _ => {
                tracing::trace!("[SEND] Forwarding other event: {:?}", event);
                let _ = self.event_sender.send(event);
            }
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<TransportEvent> {
        self.event_sender.subscribe()
    }

    /// Send data packet and wait for response (with options)
    pub async fn request_with_options(
        &self,
        data: Bytes,
        options: super::TransportOptions,
    ) -> Result<Bytes, TransportError> {
        // Use user-provided message_id or generate new one
        let message_id = options.message_id.unwrap_or_else(|| {
            self.request_tracker
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        });

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
            payload: data.to_vec(),
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
        let (_id, rx) = self.request_tracker.register_with_id(message_id);

        tracing::info!(
            "[SEND] Sending request: message_id={}, biz_type={}, timeout={:?}",
            message_id,
            packet.header.biz_type,
            options.timeout
        );

        // Send packet
        self.send(packet).await?;

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
                match self.decode_payload(&resp) {
                    Ok(decoded_data) => Ok(Bytes::from(decoded_data)),
                    Err(e) => {
                        tracing::warn!(
                            "[WARN] Failed to decompress response data: {}, using raw data",
                            e
                        );
                        Ok(Bytes::from(resp.payload))
                    }
                }
            }
            Ok(Err(_)) => {
                tracing::warn!("[WARN] Response channel closed: message_id={}", message_id);
                Err(TransportError::connection_error("Connection closed", false))
            }
            Err(_) => {
                tracing::warn!(
                    "[WARN] Request timeout: message_id={}, timeout={:?}",
                    message_id,
                    timeout_duration
                );
                Err(TransportError::connection_error("Request timeout", false))
            }
        }
    }

    /// Send one-way message (with options)
    pub async fn send_with_options(
        &self,
        data: Bytes,
        options: super::TransportOptions,
    ) -> Result<(), TransportError> {
        // Use user-provided message_id or generate new one
        let message_id = options.message_id.unwrap_or_else(|| {
            self.request_tracker
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        });

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
            payload: data.to_vec(),
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
            connection_pool: self.connection_pool.clone(),
            memory_pool: self.memory_pool.clone(),
            // [FIX] Clone should share connection state, not reset it
            connection_adapter: self.connection_adapter.clone(),
            session_id: self.session_id.clone(),
            state_manager: self.state_manager.clone(),
            event_sender: self.event_sender.clone(),
            request_tracker: self.request_tracker.clone(),
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

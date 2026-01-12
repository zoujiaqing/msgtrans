/// Server-side transport layer implementation
/// 
/// Provides multi-protocol server support, managing sessions and connections

use std::sync::Arc;
use crate::{
    SessionId, TransportError, Packet,
    transport::{
        config::TransportConfig,
        lockfree::LockFreeHashMap,
        connection_state::ConnectionStateManager,
    },
    command::TransportStats,

    event::ServerEvent,

};
use tokio::sync::broadcast;

/// TransportServer - multi-protocol server
/// 
/// [TARGET] Design goals:
/// - Multi-protocol support
/// - High-concurrency connection management
/// - Unified event system
pub struct TransportServer {
    /// Configuration
    config: TransportConfig,
    /// [CORE] Session to transport layer mapping (unified using Transport abstraction)
    transports: Arc<LockFreeHashMap<SessionId, Arc<crate::transport::transport::Transport>>>,
    /// Session ID generator
    session_id_generator: Arc<std::sync::atomic::AtomicU64>,
    /// Server statistics (using lockfree)
    stats: Arc<LockFreeHashMap<SessionId, TransportStats>>,
    /// Event broadcaster
    event_sender: broadcast::Sender<ServerEvent>,
    /// Whether it's running
    is_running: Arc<std::sync::atomic::AtomicBool>,
    /// [CONFIG] Protocol configuration - changed to server-specific configuration
    protocol_configs: std::collections::HashMap<String, Box<dyn crate::protocol::adapter::DynServerConfig>>,
    /// Connection state manager
    state_manager: ConnectionStateManager,
    /// [NEW] Request tracker - supports server sending requests to clients
    request_tracker: Arc<crate::transport::transport::RequestTracker>,
    /// [NEW] Message ID counter - for automatic message ID generation
    message_id_counter: std::sync::atomic::AtomicU32,
}

impl TransportServer {
    /// Create new TransportServer
    pub async fn new(config: TransportConfig) -> Result<Self, TransportError> {
        let (event_sender, _) = broadcast::channel(1000);
        
        Ok(Self {
            config,
            transports: Arc::new(LockFreeHashMap::new()),
            session_id_generator: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            stats: Arc::new(LockFreeHashMap::new()),
            event_sender,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            protocol_configs: std::collections::HashMap::new(),
            state_manager: ConnectionStateManager::new(),
            request_tracker: Arc::new(crate::transport::transport::RequestTracker::new_with_start_id(10000)),
            message_id_counter: std::sync::atomic::AtomicU32::new(20000), // Server uses higher ID range
        })
    }

    /// [INTERNAL] Internal method: create server with protocol configuration (called by TransportServerBuilder)
    pub async fn new_with_protocols(
        config: TransportConfig,
        protocol_configs: std::collections::HashMap<String, Box<dyn crate::protocol::adapter::DynServerConfig>>
    ) -> Result<Self, TransportError> {
        let (event_sender, _) = broadcast::channel(1000);
        
        Ok(Self {
            config,
            transports: Arc::new(LockFreeHashMap::new()),
            session_id_generator: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            stats: Arc::new(LockFreeHashMap::new()),
            event_sender,
            is_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            protocol_configs,
            state_manager: ConnectionStateManager::new(),
            request_tracker: Arc::new(crate::transport::transport::RequestTracker::new_with_start_id(10000)),
            message_id_counter: std::sync::atomic::AtomicU32::new(20000), // Server uses higher ID range
        })
    }

    /// [LOCKFREE] Send packet to specified session - lock-free version
    pub async fn send_to_session(&self, session_id: SessionId, packet: Packet) -> Result<(), TransportError> {
        tracing::debug!("[SEND] TransportServer sending packet to session {} (ID: {}, size: {} bytes)", 
            session_id, packet.header.message_id, packet.payload.len());
        
        if let Some(transport) = self.transports.get(&session_id) {
            // [STATUS] Status check - through Transport abstraction
            if !transport.is_connected().await {
                tracing::warn!("[WARN] Session {} connection closed, skipping send", session_id);
                // Clean up disconnected connection
                let _ = self.remove_session(session_id).await;
                return Err(TransportError::connection_error("Connection closed", false));
            }
            
            tracing::debug!("[CHECK] Session {} connection status normal, starting packet send", session_id);
            
            // [SEND] Send through Transport unified interface
            match transport.send(packet).await {
                Ok(()) => {
                    tracing::debug!("[SUCCESS] Session {} send successful (TransportServer layer confirmation)", session_id);
                    Ok(())
                }
                Err(e) => {
                    tracing::error!("[ERROR] Session {} send failed: {:?}", session_id, e);
                    
                    // [FIX] Critical fix: check if it's a connection-related error
                    let error_msg = format!("{:?}", e);
                    if error_msg.contains("Broken pipe") || 
                       error_msg.contains("Connection reset") || 
                       error_msg.contains("Connection closed") ||
                       error_msg.contains("ECONNRESET") ||
                       error_msg.contains("EPIPE") {
                        tracing::warn!("[WARN] Session {} connection closed: {}", session_id, error_msg);
                        // Clean up disconnected connection
                        let _ = self.remove_session(session_id).await;
                        return Err(TransportError::connection_error("Connection closed during send", false));
                    } else {
                        tracing::error!("[ERROR] Session {} send failed (non-connection error): {:?}", session_id, e);
                        return Err(e);
                    }
                }
            }
        } else {
            tracing::warn!("[WARN] Session {} does not exist in connection mapping", session_id);
            Err(TransportError::connection_error("Session not found", false))
        }
    }

    /// [REQUEST] Send request to specified session and wait for response - using Transport's request method
    pub async fn request_to_session(&self, session_id: SessionId, packet: Packet) -> Result<Packet, TransportError> {
        tracing::debug!("[REQUEST] TransportServer sending request to session {} (ID: {})", session_id, packet.header.message_id);
        
        if let Some(transport) = self.transports.get(&session_id) {
            // [STATUS] Status check - through Transport abstraction
            if !transport.is_connected().await {
                tracing::warn!("[WARN] Session {} connection closed, cannot send request", session_id);
                let _ = self.remove_session(session_id).await;
                return Err(TransportError::connection_error("Connection closed", false));
            }
            
            // [DIRECT] Directly use Transport's request method, it will correctly manage RequestTracker
            match transport.request(packet).await {
                Ok(response) => {
                    tracing::debug!("[SUCCESS] Session {} received response (response ID: {})", session_id, response.header.message_id);
                    Ok(response)
                }
                Err(e) => {
                    tracing::error!("[ERROR] Session {} request failed: {:?}", session_id, e);
                    Err(e)
                }
            }
        } else {
            tracing::warn!("[WARN] Session {} does not exist in connection mapping", session_id);
            Err(TransportError::connection_error("Session not found", false))
        }
    }

    /// [UNIFIED] Send byte data to specified session - unified API returns TransportResult
    pub async fn send(&self, session_id: SessionId, data: &[u8]) -> Result<crate::event::TransportResult, TransportError> {
        let message_id = self.message_id_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let packet = crate::packet::Packet::one_way(message_id, data.to_vec());
        
        tracing::debug!("TransportServer sending data to session {}: {} bytes (ID: {})", session_id, data.len(), message_id);
        
        match self.send_to_session(session_id, packet).await {
            Ok(()) => {
                // Send successful, return TransportResult
                Ok(crate::event::TransportResult::new_sent(Some(session_id), message_id))
            }
            Err(e) => Err(e),
        }
    }
    
    /// [UNIFIED] Send byte request to specified session and wait for response - unified API returns TransportResult
    pub async fn request(&self, session_id: SessionId, data: &[u8]) -> Result<crate::event::TransportResult, TransportError> {
        let message_id = self.message_id_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let packet = crate::packet::Packet::request(message_id, data.to_vec());
        
        tracing::debug!("TransportServer sending request to session {}: {} bytes (ID: {})", session_id, data.len(), message_id);
        
        match self.request_to_session(session_id, packet).await {
            Ok(response_packet) => {
                tracing::debug!("TransportServer received response from session {}: {} bytes (ID: {})", session_id, response_packet.payload.len(), response_packet.header.message_id);
                // Request successful, return TransportResult containing response data
                Ok(crate::event::TransportResult::new_completed(Some(session_id), message_id, response_packet.payload.clone()))
            }
            Err(e) => {
                // Check if it's a timeout error
                if e.to_string().contains("timeout") || e.to_string().contains("Timeout") {
                    Ok(crate::event::TransportResult::new_timeout(Some(session_id), message_id))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// [ABSTRACT] Add session - using Transport abstraction
    pub async fn add_session(&self, connection: Box<dyn crate::Connection>) -> SessionId {
        // [FIX] Use existing session ID from connection instead of generating new one
        let session_id = connection.session_id();
        
        // [CREATE] Create Transport instance
        let transport = Arc::new(crate::transport::transport::Transport::new(self.config.clone()).await.unwrap());
        
        // [FIX] Subscribe to event stream BEFORE setting connection
        // This ensures that when adapter's event loop starts, there's already a subscriber ready
        // This prevents message loss during the time window between event loop start and consumer loop start
        let event_receiver_opt = connection.event_stream();
        
        // Set connection to Transport
        transport.set_connection(connection, session_id).await;
        
        // Insert into transport layer mapping
        self.transports.insert(session_id, transport.clone());
        self.stats.insert(session_id, TransportStats::new());
        
        // Register connection state
        self.state_manager.add_connection(session_id);
        
        // [LOOP] Start event consumption loop, converting TransportEvent to ServerEvent
        // [FIX] Use the pre-subscribed event receiver to ensure no messages are lost
        if let Some(mut event_receiver) = event_receiver_opt {
            let server_clone = self.clone();
            tokio::spawn(async move {
                tracing::info!("[LISTENER] TransportServer starting event consumption loop for session {} (pre-subscribed)", session_id);
                while let Ok(transport_event) = event_receiver.recv().await {
                    tracing::trace!("[EVENT] TransportServer received event from session {}: {:?}", session_id, transport_event);
                    server_clone.handle_transport_event(session_id, transport_event).await;
                }
                tracing::info!("[END] TransportServer event consumption loop ended for session {}", session_id);
            });
        } else {
            tracing::warn!("[WARN] Session {} unable to get event stream before connection setup", session_id);
        }
        
        tracing::info!("[SUCCESS] TransportServer added session: {} (using Transport abstraction)", session_id);
        session_id
    }

    /// Remove session
    pub async fn remove_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        self.transports.remove(&session_id);
        self.stats.remove(&session_id);
        self.state_manager.remove_connection(session_id);
        tracing::info!("[REMOVE] TransportServer removed session: {}", session_id);
        Ok(())
    }
    
    /// [UNIFIED] Unified close method: graceful session close
    pub async fn close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        // 1. Check if close can be started
        if !self.state_manager.try_start_closing(session_id).await {
            tracing::debug!("Session {} already closing or closed, skipping close logic", session_id);
            return Ok(());
        }
        
        tracing::info!("[CLOSE] Starting graceful close for session: {}", session_id);
        
        // 2. Send connection close event (before resource cleanup)
        let close_event = ServerEvent::ConnectionClosed {
            session_id,
            reason: crate::error::CloseReason::Normal,
        };
        let _ = self.event_sender.send(close_event);
        
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
            tracing::debug!("Session {} already closing or closed, skipping force close", session_id);
            return Ok(());
        }
        
        tracing::info!("[FORCE] Force closing session: {}", session_id);
        
        // 2. Send connection close event
        let close_event = ServerEvent::ConnectionClosed {
            session_id,
            reason: crate::error::CloseReason::Forced,
        };
        let _ = self.event_sender.send(close_event);
        
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
        
        tracing::info!("[BATCH] Starting batch close of {} sessions", total_sessions);
        
        // Use graceful_timeout as total timeout for batch close
        let start_time = std::time::Instant::now();
        let timeout = self.config.graceful_timeout;
        
        let mut success_count = 0;
        let mut error_count = 0;
        
        for session_id in session_ids {
            // Check if timeout
            if start_time.elapsed() >= timeout {
                tracing::warn!("[WARN] Batch close timeout, remaining sessions will be force closed");
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
        
        tracing::info!("[SUCCESS] Batch close completed, success: {}, failed: {}", success_count, error_count);
        Ok(())
    }
    
    /// Internal method: execute actual close logic - through Transport abstraction
    async fn do_close_session(&self, session_id: SessionId) -> Result<(), TransportError> {
        if let Some(transport) = self.transports.get(&session_id) {
            // Try graceful close
            match tokio::time::timeout(
                self.config.graceful_timeout,
                transport.disconnect()
            ).await {
                Ok(Ok(_)) => {
                    tracing::debug!("[SUCCESS] Session {} graceful close successful", session_id);
                }
                Ok(Err(e)) => {
                    tracing::warn!("[WARN] Session {} graceful close failed: {:?}", session_id, e);
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
    pub async fn broadcast(&self, packet: Packet) -> Result<(), TransportError> {
        let mut success_count = 0;
        let mut error_count = 0;
        
        // First collect all session IDs, then process one by one
        let session_ids: Vec<SessionId> = self.transports.keys().unwrap_or_default();
        for session_id in session_ids {
            if let Some(transport) = self.transports.get(&session_id) {
                match transport.send(packet.clone()).await {
                    Ok(()) => success_count += 1,
                    Err(e) => {
                        error_count += 1;
                        tracing::warn!("[WARN] Broadcast to session {} failed: {:?}", session_id, e);
                    }
                }
            }
        }
        
        if error_count > 0 {
            tracing::warn!("[WARN] Broadcast completed, success: {}, failed: {}", success_count, error_count);
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
        let id = self.session_id_generator.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SessionId(id)
    }

    /// Business layer subscribe to ServerEvent
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<crate::event::ServerEvent> {
        self.event_sender.subscribe()
    }

    /// Start server
    pub async fn serve(&self) -> Result<(), TransportError> {
        self.is_running.store(true, std::sync::atomic::Ordering::SeqCst);
        
        if self.protocol_configs.is_empty() {
            tracing::warn!("[WARN] No protocols configured, server cannot start listening");
            return Err(TransportError::config_error("protocols", "No protocols configured"));
        }
        
        tracing::info!("[START] Starting {} protocol servers", self.protocol_configs.len());
        
        // Create vector of listen tasks
        let mut listen_tasks = Vec::new();
        
        // Start server for each protocol configuration
        for (protocol_name, protocol_config) in &self.protocol_configs {
            tracing::info!("[CONFIG] Processing protocol: {}", protocol_name);
            
            let address = self.get_protocol_bind_address(protocol_config);
            tracing::info!("[BIND] Protocol {} bind address: {}", protocol_name, address);
            
            match protocol_config.build_server_dyn().await {
                Ok(server) => {
                    match self.start_protocol_listener(server, protocol_name.clone()).await {
                        Ok(listener_task) => {
                            listen_tasks.push(listener_task);
                            tracing::info!("[SUCCESS] {} server started successfully: {}", protocol_name, address);
                        }
                        Err(e) => {
                            tracing::error!("[ERROR] {} listener task creation failed: {:?}", protocol_name, e);
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("[ERROR] {} server build failed: {:?}", protocol_name, e);
                    return Err(e);
                }
            }
        }
        
        tracing::info!("[TARGET] All protocol servers started, waiting for connections...");
        
        // Wait for all listen tasks to complete
        for (index, task) in listen_tasks.into_iter().enumerate() {
            tracing::info!("[WAIT] Waiting for task {} to complete...", index + 1);
            if let Err(e) = task.await {
                tracing::error!("[ERROR] Task {} was cancelled: {:?}", index + 1, e);
                return Err(TransportError::config_error("server", "Listener task cancelled"));
            }
        }
        
        tracing::info!("[STOP] TransportServer stopped");
        Ok(())
    }

    /// [START] Start protocol listener - generic method
    async fn start_protocol_listener(&self, mut server: Box<dyn crate::Server>, protocol_name: String) -> Result<tokio::task::JoinHandle<()>, TransportError>
    {
        let server_clone = self.clone();
        
        let task = tokio::spawn(async move {
            tracing::info!("[START] {} listener task started", protocol_name);
            
            let mut accept_count = 0u64;
            
            loop {
                tracing::debug!("[LOOP] {} waiting for connections... (accept count: {})", protocol_name, accept_count);
                
                match server.accept().await {
                    Ok(mut connection) => {
                        accept_count += 1;
                        tracing::info!("[SUCCESS] {} accept successful! Connection #{}", protocol_name, accept_count);
                        
                        // Get connection info
                        let connection_info = connection.connection_info();
                        let peer_addr = connection_info.peer_addr;
                        
                        tracing::info!("[CONNECT] New {} connection #{}: {}", protocol_name, accept_count, peer_addr);
                        
                        // Generate new session ID and set to connection
                        let session_id = server_clone.generate_session_id();
                        connection.set_session_id(session_id);
                        tracing::info!("[ID] Generated session ID for {} connection: {}", protocol_name, session_id);
                        
                        // Add to session management
                        let actual_session_id = server_clone.add_session(connection).await;
                        
                        // Send connection established event
                        let connect_event = ServerEvent::ConnectionEstablished { 
                            session_id: actual_session_id,
                            info: connection_info,
                        };
                        let _ = server_clone.event_sender.send(connect_event);
                        tracing::info!("[EVENT] {} connection event sent", protocol_name);
                    }
                    Err(e) => {
                        tracing::error!("[ERROR] {} accept connection failed: {:?}", protocol_name, e);
                        break;
                    }
                }
            }
            
            tracing::info!("[STOP] {} server stopped", protocol_name);
        });
        
        Ok(task)
    }

    /// [INTERNAL] Internal method: extract listen address from protocol configuration
    fn get_protocol_bind_address(&self, protocol_config: &Box<dyn crate::protocol::adapter::DynServerConfig>) -> std::net::SocketAddr {
        protocol_config.get_bind_address()
    }

    /// [HANDLER] Handle connection's TransportEvent, convert to ServerEvent
    async fn handle_transport_event(&self, session_id: SessionId, transport_event: crate::event::TransportEvent) {
        tracing::info!("[HANDLER] handle_transport_event: session {}, event: {:?}", session_id, transport_event);
        match transport_event {
            crate::event::TransportEvent::MessageReceived(packet) => {
                tracing::info!("[RECV] Received MessageReceived event, packet type: {:?}, ID: {}, biz_type: {}", packet.header.packet_type, packet.header.message_id, packet.header.biz_type);
                match packet.header.packet_type {
                    crate::packet::PacketType::Request => {
                        tracing::info!("[REQUEST] Handling Request type packet (ID: {}, biz_type: {})", packet.header.message_id, packet.header.biz_type);
                        // Create unified TransportContext
                        let server_clone = self.clone();
                        let mut context = crate::event::TransportContext::new_request(
                            Some(session_id),
                            packet.header.message_id,
                            packet.header.biz_type,
                            if packet.ext_header.is_empty() { None } else { Some(packet.ext_header.clone()) },
                            packet.payload.clone(),
                            std::sync::Arc::new(move |response_data: Vec<u8>| {
                                let server = server_clone.clone();
                                let request_message_id = packet.header.message_id;
                                let request_biz_type = packet.header.biz_type;
                                tokio::spawn(async move {
                                    tracing::info!("[RESPOND] Creating response packet: request_id={}, request_biz_type={}, response_size={} bytes", request_message_id, request_biz_type, response_data.len());
                                    let response_packet = crate::packet::Packet {
                                        header: crate::packet::FixedHeader {
                                            version: 1,
                                            compression: crate::packet::CompressionType::None,
                                            packet_type: crate::packet::PacketType::Response,
                                            biz_type: 0,
                                            message_id: request_message_id,
                                            ext_header_len: 0,
                                            payload_len: response_data.len() as u32,
                                            reserved: crate::packet::ReservedFlags::new(),
                                        },
                                        ext_header: Vec::new(),
                                        payload: response_data,
                                    };
                                    match server.send_to_session(session_id, response_packet).await {
                                        Ok(_) => {
                                            tracing::info!("[SUCCESS] Response sent successfully: session={}, request_id={}", session_id, request_message_id);
                                        }
                                        Err(e) => {
                                            tracing::error!("[ERROR] Failed to send response: session={}, request_id={}, error={:?}", session_id, request_message_id, e);
                                        }
                                    }
                                });
                            }),
                        );
                        
                        // [FIX] Fix BUG: set as primary instance, ensure request is correctly tracked and responded
                        context.set_primary();
                        tracing::debug!("[SUCCESS] Set TransportContext as primary instance (ID: {})", packet.header.message_id);
                        
                        let event = crate::event::ServerEvent::MessageReceived { 
                            session_id, 
                            context 
                        };
                        tracing::info!("[SEND] Preparing to send ServerEvent::MessageReceived (session: {}, ID: {}, biz_type: {})", session_id, packet.header.message_id, packet.header.biz_type);
                        
                        match self.event_sender.send(event) {
                            Ok(receivers) => {
                                tracing::info!("[SUCCESS] ServerEvent sent successfully, receiver count: {} (session: {}, ID: {})", receivers, session_id, packet.header.message_id);
                            }
                            Err(e) => {
                                tracing::error!("[ERROR] ServerEvent send failed: {:?} (session: {}, ID: {})", e, session_id, packet.header.message_id);
                            }
                        }
                    }
                    crate::packet::PacketType::Response => {
                        // [NEW] Handle response packet - complete server-initiated request
                        let message_id = packet.header.message_id;
                        tracing::debug!("[RECV] TransportServer received response packet from session {} (ID: {})", session_id, message_id);
                        
                        if self.request_tracker.complete(message_id, packet.clone()) {
                            tracing::debug!("[SUCCESS] Successfully completed server request (ID: {})", message_id);
                        } else {
                            tracing::warn!("[WARN] Received unknown response packet (ID: {}), may be timeout or duplicate response", message_id);
                            // Process as normal message
                            let context = crate::event::TransportContext::new_oneway(
                                Some(session_id),
                                packet.header.message_id,
                                packet.header.biz_type,
                                if packet.ext_header.is_empty() { None } else { Some(packet.ext_header.clone()) },
                                packet.payload.clone(),
                            );
                            let event = crate::event::ServerEvent::MessageReceived { session_id, context };
                            match self.event_sender.send(event) {
                                Ok(receivers) => {
                                    tracing::debug!("[SUCCESS] Unknown response packet sent as normal message successfully, receiver count: {}", receivers);
                                }
                                Err(e) => {
                                    tracing::error!("[ERROR] Unknown response packet as normal message send failed: {:?}", e);
                                }
                            }
                        }
                    }
                    _ => {
                        // Other types of packets processed as normal messages
                        tracing::debug!("[PACKET] Processing other type packet (type: {:?}, ID: {})", packet.header.packet_type, packet.header.message_id);
                        let context = crate::event::TransportContext::new_oneway(
                            Some(session_id),
                            packet.header.message_id,
                            packet.header.biz_type,
                            if packet.ext_header.is_empty() { None } else { Some(packet.ext_header.clone()) },
                            packet.payload.clone(),
                        );
                        let event = crate::event::ServerEvent::MessageReceived { session_id, context };
                        match self.event_sender.send(event) {
                            Ok(receivers) => {
                                tracing::debug!("[SUCCESS] Other type packet sent successfully, receiver count: {}", receivers);
                            }
                            Err(e) => {
                                tracing::error!("[ERROR] Other type packet send failed: {:?}", e);
                            }
                        }
                    }
                }
            }
            crate::event::TransportEvent::MessageSent { packet_id } => {
                tracing::debug!("[SEND] Processing MessageSent event (ID: {})", packet_id);
                let event = crate::event::ServerEvent::MessageSent { session_id, message_id: packet_id };
                match self.event_sender.send(event) {
                    Ok(receivers) => {
                        tracing::debug!("[SUCCESS] MessageSent event sent successfully, receiver count: {}", receivers);
                    }
                    Err(e) => {
                        tracing::error!("[ERROR] MessageSent event send failed: {:?}", e);
                    }
                }
            }
            crate::event::TransportEvent::ConnectionClosed { reason } => {
                tracing::debug!("[CLOSE] Processing ConnectionClosed event, reason: {:?}", reason);
                let event = crate::event::ServerEvent::ConnectionClosed { session_id, reason };
                match self.event_sender.send(event) {
                    Ok(receivers) => {
                        tracing::debug!("[SUCCESS] ConnectionClosed event sent successfully, receiver count: {}", receivers);
                    }
                    Err(e) => {
                        tracing::error!("[ERROR] ConnectionClosed event send failed: {:?}", e);
                    }
                }
                // Clean up session
                let _ = self.remove_session(session_id).await;
            }
            crate::event::TransportEvent::TransportError { error } => {
                tracing::debug!("[ERROR] Processing TransportError event: {:?}", error);
                let event = crate::event::ServerEvent::TransportError { 
                    session_id: Some(session_id), 
                    error 
                };
                match self.event_sender.send(event) {
                    Ok(receivers) => {
                        tracing::debug!("[SUCCESS] TransportError event sent successfully, receiver count: {}", receivers);
                    }
                    Err(e) => {
                        tracing::error!("[ERROR] TransportError event send failed: {:?}", e);
                    }
                }
            }
            // Other events temporarily ignored or logged
            _ => {
                tracing::trace!("[IGNORE] TransportServer ignoring event: {:?}", transport_event);
            }
        }
    }

    /// [STOP] Stop server
    pub async fn stop(&self) {
        tracing::info!("[STOP] Stopping TransportServer");
        self.is_running.store(false, std::sync::atomic::Ordering::SeqCst);
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
            transports: self.transports.clone(),
            session_id_generator: self.session_id_generator.clone(),
            stats: self.stats.clone(),
            event_sender: self.event_sender.clone(),
            is_running: self.is_running.clone(),
            protocol_configs: cloned_configs,
            state_manager: self.state_manager.clone(),
            request_tracker: self.request_tracker.clone(),
            message_id_counter: std::sync::atomic::AtomicU32::new(20000), // Re-initialize on clone
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

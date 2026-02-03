use bytes::Bytes;
use std::sync::Arc;
/// Client transport layer module
///
/// Provides transport layer API specifically designed for client connections
use std::time::Duration;
use tokio::sync::RwLock;

use crate::{
    error::TransportError, protocol::adapter::DynClientConfig, transport::config::TransportConfig,
    SessionId,
};

// Internal use of new Transport structure
use super::transport::Transport;

/// Connection config trait - Local definition
pub trait ConnectableConfig {
    async fn connect(&self, transport: &mut Transport) -> Result<SessionId, TransportError>;
    fn validate(&self) -> Result<(), TransportError>;
    fn protocol_name(&self) -> &'static str;
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Connection pool configuration
#[derive(Debug, Clone)]
pub struct ConnectionPoolConfig {
    pub max_size: usize,
    pub idle_timeout: Duration,
    pub health_check_interval: Duration,
    pub min_idle: usize,
}

impl Default for ConnectionPoolConfig {
    fn default() -> Self {
        Self {
            max_size: 100,
            idle_timeout: Duration::from_secs(300),
            health_check_interval: Duration::from_secs(30),
            min_idle: 5,
        }
    }
}

/// Retry configuration
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_multiplier: f64,
}

impl RetryConfig {
    pub fn exponential_backoff(max_retries: usize, initial_delay: Duration) -> Self {
        Self {
            max_retries,
            initial_delay,
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_multiplier: 2.0,
        }
    }
}

/// Load balancer configuration
#[derive(Debug, Clone)]
pub enum LoadBalancerConfig {
    RoundRobin,
    Random,
    LeastConnections,
    WeightedRoundRobin(Vec<u32>),
}

/// Circuit breaker configuration
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    pub failure_threshold: usize,
    pub timeout: Duration,
    pub success_threshold: usize,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            timeout: Duration::from_secs(60),
            success_threshold: 3,
        }
    }
}

/// Connection options
#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    pub timeout: Option<Duration>,
    pub max_retries: usize,
    pub priority: ConnectionPriority,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            max_retries: 0,
            priority: ConnectionPriority::Normal,
        }
    }
}

/// Connection priority
#[derive(Debug, Clone)]
pub enum ConnectionPriority {
    Low,
    Normal,
    High,
    Critical,
}

/// Client transport layer builder
pub struct TransportClientBuilder {
    connect_timeout: Duration,
    pool_config: ConnectionPoolConfig,
    retry_config: RetryConfig,
    load_balancer: Option<LoadBalancerConfig>,
    circuit_breaker: Option<CircuitBreakerConfig>,
    connection_monitoring: bool,
    transport_config: TransportConfig,
    /// Protocol configuration storage - Client only supports one protocol connection
    protocol_config: Option<Box<dyn DynClientConfig>>,
}

impl TransportClientBuilder {
    pub fn new() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            pool_config: ConnectionPoolConfig::default(),
            retry_config: RetryConfig::default(),
            load_balancer: None,
            circuit_breaker: None,
            connection_monitoring: false,
            transport_config: TransportConfig::default(),
            protocol_config: None,
        }
    }

    /// Set protocol configuration - Client specific
    pub fn with_protocol<T: DynClientConfig>(mut self, config: T) -> Self {
        self.protocol_config = Some(Box::new(config));
        self
    }

    /// Client specific: Connection timeout
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Client specific: Connection pool configuration
    pub fn connection_pool(mut self, config: ConnectionPoolConfig) -> Self {
        self.pool_config = config;
        self
    }

    /// Client specific: Retry strategy
    pub fn retry_strategy(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// Client specific: Load balancer
    pub fn load_balancer(mut self, config: LoadBalancerConfig) -> Self {
        self.load_balancer = Some(config);
        self
    }

    /// Client specific: Circuit breaker
    pub fn circuit_breaker(mut self, config: CircuitBreakerConfig) -> Self {
        self.circuit_breaker = Some(config);
        self
    }

    /// Client specific: Connection monitoring
    pub fn enable_connection_monitoring(mut self, enabled: bool) -> Self {
        self.connection_monitoring = enabled;
        self
    }

    /// Set transport layer basic configuration
    pub fn transport_config(mut self, config: TransportConfig) -> Self {
        self.transport_config = config;
        self
    }

    /// Build client transport layer - return TransportClient
    pub async fn build(self) -> Result<TransportClient, TransportError> {
        // Create underlying Transport
        let transport = Transport::new(self.transport_config).await?;

        Ok(TransportClient::new(
            transport,
            self.retry_config,
            self.protocol_config,
        ))
    }
}

impl Default for TransportClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// [TARGET] Transport layer client - Uses Transport for single connection management
pub struct TransportClient {
    inner: Arc<Transport>,
    retry_config: RetryConfig,
    // Client protocol configuration
    protocol_config: Option<Box<dyn DynClientConfig>>,
    // [TARGET] Current connection session ID - Uses Arc<RwLock> for modification
    current_session_id: Arc<RwLock<Option<SessionId>>>,
    event_sender: tokio::sync::broadcast::Sender<crate::event::ClientEvent>,
    // [TARGET] Message ID counter - For automatic message ID generation
    message_id_counter: std::sync::atomic::AtomicU32,
}

impl TransportClient {
    pub(crate) fn new(
        transport: Transport,
        retry_config: RetryConfig,
        protocol_config: Option<Box<dyn DynClientConfig>>,
    ) -> Self {
        Self {
            inner: Arc::new(transport),
            retry_config,
            protocol_config,
            current_session_id: Arc::new(RwLock::new(None)),
            event_sender: tokio::sync::broadcast::channel(8192).0,
            message_id_counter: std::sync::atomic::AtomicU32::new(1),
        }
    }

    /// [CONNECT] Use protocol configuration specified at build time for connection - Framework's only connection method
    pub async fn connect(&mut self) -> Result<(), TransportError> {
        // Check if protocol configuration exists and clone to avoid borrow conflicts
        let protocol_config = self.protocol_config.as_ref()
            .ok_or_else(|| TransportError::config_error("protocol",
                "No protocol config specified. Use TransportClientBuilder::with_protocol() when building."))?
            .clone_client_dyn();

        // Validate protocol configuration
        protocol_config.validate_dyn().map_err(|e| {
            TransportError::config_error("protocol", format!("Config validation failed: {:?}", e))
        })?;

        // Connect using stored protocol configuration
        let session_id = self.connect_with_stored_config(&protocol_config).await?;

        // Update current session ID (internal use)
        let mut current_session = self.current_session_id.write().await;
        *current_session = Some(session_id);

        // [START] Start event forwarding task, converting Transport events to ClientEvent
        self.start_event_forwarding().await?;

        tracing::info!("[SUCCESS] TransportClient connected successfully");
        Ok(())
    }

    /// [CONFIG] Internal method: Connect using stored protocol configuration
    async fn connect_with_stored_config(
        &mut self,
        protocol_config: &Box<dyn DynClientConfig>,
    ) -> Result<SessionId, TransportError> {
        let mut last_error = None;
        let max_retries = self.retry_config.max_retries;

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = self.calculate_retry_delay(attempt);
                tracing::debug!(
                    "Connection retry {}/{}, delay: {:?}",
                    attempt,
                    max_retries,
                    delay
                );
                tokio::time::sleep(delay).await;
            }

            // Connect according to protocol type
            match protocol_config.protocol_name() {
                "tcp" => {
                    if let Some(tcp_config) = protocol_config
                        .as_any()
                        .downcast_ref::<crate::protocol::TcpClientConfig>()
                    {
                        match self.inner.connect_with_config(tcp_config.clone()).await {
                            Ok(session_id) => return Ok(session_id),
                            Err(e) => {
                                last_error = Some(e);
                                tracing::warn!(
                                    "TCP connection failed (attempt {}): {:?}",
                                    attempt + 1,
                                    last_error
                                );
                            }
                        }
                    } else {
                        return Err(TransportError::config_error(
                            "protocol",
                            "Invalid TCP config",
                        ));
                    }
                }
                "websocket" => {
                    if let Some(ws_config) = protocol_config
                        .as_any()
                        .downcast_ref::<crate::protocol::WebSocketClientConfig>(
                    ) {
                        match self.inner.connect_with_config(ws_config.clone()).await {
                            Ok(session_id) => return Ok(session_id),
                            Err(e) => {
                                last_error = Some(e);
                                tracing::warn!(
                                    "WebSocket connection failed (attempt {}): {:?}",
                                    attempt + 1,
                                    last_error
                                );
                            }
                        }
                    } else {
                        return Err(TransportError::config_error(
                            "protocol",
                            "Invalid WebSocket config",
                        ));
                    }
                }
                "quic" => {
                    if let Some(quic_config) = protocol_config
                        .as_any()
                        .downcast_ref::<crate::protocol::QuicClientConfig>()
                    {
                        match self.inner.connect_with_config(quic_config.clone()).await {
                            Ok(session_id) => return Ok(session_id),
                            Err(e) => {
                                last_error = Some(e);
                                tracing::warn!(
                                    "QUIC connection failed (attempt {}): {:?}",
                                    attempt + 1,
                                    last_error
                                );
                            }
                        }
                    } else {
                        return Err(TransportError::config_error(
                            "protocol",
                            "Invalid QUIC config",
                        ));
                    }
                }
                protocol_name => {
                    return Err(TransportError::config_error(
                        "protocol",
                        format!("Unsupported protocol: {}", protocol_name),
                    ));
                }
            }
        }

        // All retries failed
        Err(last_error.unwrap_or_else(|| {
            TransportError::connection_error("Connection failed after all retries", true)
        }))
    }

    fn calculate_retry_delay(&self, attempt: usize) -> std::time::Duration {
        let delay = self.retry_config.initial_delay.as_secs_f64()
            * self.retry_config.backoff_multiplier.powi(attempt as i32);
        let delay = delay.min(self.retry_config.max_delay.as_secs_f64());
        std::time::Duration::from_secs_f64(delay)
    }

    /// [DISCONNECT] Disconnect (graceful close)
    pub async fn disconnect(&self) -> Result<(), TransportError> {
        // Check if already connected
        let mut current_session = self.current_session_id.write().await;
        if let Some(session_id) = current_session.take() {
            drop(current_session);

            tracing::info!("TransportClient disconnecting");

            // Use Transport's unified close method
            self.inner.close_session(session_id).await?;

            Ok(())
        } else {
            Err(TransportError::connection_error("Not connected", false))
        }
    }

    /// [FORCE] Force disconnect
    pub async fn force_disconnect(&self) -> Result<(), TransportError> {
        // Check if already connected
        let mut current_session = self.current_session_id.write().await;
        if let Some(session_id) = current_session.take() {
            drop(current_session);

            tracing::info!("TransportClient force disconnecting");

            // Use Transport's force close method
            self.inner.force_close_session(session_id).await?;

            Ok(())
        } else {
            Err(TransportError::connection_error("Not connected", false))
        }
    }

    /// [SEND] Send byte data - Unified API returns TransportResult
    pub async fn send(&self, data: &[u8]) -> Result<crate::event::TransportResult, TransportError> {
        if !self.is_connected().await {
            return Err(TransportError::connection_error(
                "Not connected - call connect() first",
                false,
            ));
        }

        let message_id = self
            .message_id_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let packet = crate::packet::Packet::one_way(message_id, data.to_vec());

        tracing::debug!(
            "TransportClient sending data: {} bytes (ID: {})",
            data.len(),
            message_id
        );

        match self.inner.send(packet).await {
            Ok(()) => {
                // Send successful, return TransportResult
                Ok(crate::event::TransportResult::new_sent(None, message_id))
            }
            Err(e) => Err(e),
        }
    }

    /// [REQUEST] Send byte request and wait for response - Unified API returns TransportResult
    pub async fn request(
        &self,
        data: &[u8],
    ) -> Result<crate::event::TransportResult, TransportError> {
        if !self.is_connected().await {
            return Err(TransportError::connection_error(
                "Not connected - call connect() first",
                false,
            ));
        }

        let message_id = self
            .message_id_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let packet = crate::packet::Packet::request(message_id, data.to_vec());

        tracing::debug!(
            "TransportClient sending request: {} bytes (ID: {})",
            data.len(),
            message_id
        );

        match self.inner.request(packet).await {
            Ok(response_packet) => {
                tracing::debug!(
                    "TransportClient received response: {} bytes (ID: {})",
                    response_packet.payload.len(),
                    response_packet.header.message_id
                );
                // Request successful, return TransportResult containing response data
                Ok(crate::event::TransportResult::new_completed(
                    None,
                    message_id,
                    response_packet.payload.clone(),
                ))
            }
            Err(e) => {
                // Check if it's a timeout error
                if e.to_string().contains("timeout") || e.to_string().contains("Timeout") {
                    Ok(crate::event::TransportResult::new_timeout(None, message_id))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// [STATUS] Check connection status
    pub async fn is_connected(&self) -> bool {
        self.inner.is_connected().await
    }

    /// Get connection status information
    pub async fn connection_info(&self) -> Option<crate::command::ConnectionInfo> {
        // TODO: Implement connection information retrieval
        None
    }

    /// Get current session ID
    pub async fn current_session_id(&self) -> Option<SessionId> {
        self.inner.current_session_id().await
    }

    /// Get client event stream - Returns event stream for current connection (hides session ID)
    pub async fn events(&self) -> Result<crate::stream::ClientEventStream, TransportError> {
        use crate::stream::StreamFactory;

        // Check if connected
        if !self.is_connected().await {
            return Err(TransportError::connection_error(
                "Not connected - call connect() first",
                false,
            ));
        }

        // [FIX] Fix: Use Transport's event stream directly, no longer depends on session ID
        if let Some(event_receiver) = self.inner.get_event_stream().await {
            tracing::debug!("[SUCCESS] TransportClient got connection adapter event stream");
            tracing::debug!("[STREAM] TransportClient client event stream created");
            return Ok(StreamFactory::client_event_stream(event_receiver));
        } else {
            // If event stream cannot be obtained, return error
            return Err(TransportError::connection_error(
                "Connection does not support event streams",
                false,
            ));
        }
    }

    /// [DEBUG] Internal method: Get current session ID (for internal debugging only)
    async fn current_session(&self) -> Option<SessionId> {
        self.inner.current_session_id().await
    }

    /// Get client connection statistics
    /// TODO: Transport needs to implement statistics functionality
    pub async fn stats(&self) -> Result<crate::command::TransportStats, TransportError> {
        // Temporarily return error, waiting for Transport to implement statistics
        Err(TransportError::connection_error(
            "Stats not implemented for Transport yet",
            false,
        ))
    }

    /// Business layer subscribe to ClientEvent
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<crate::event::ClientEvent> {
        self.event_sender.subscribe()
    }

    /// [START] Start event forwarding task
    async fn start_event_forwarding(&self) -> Result<(), TransportError> {
        // Get Transport's event stream
        if let Some(mut transport_events) = self.inner.get_event_stream().await {
            let client_event_sender = self.event_sender.clone();
            let transport_for_response = self.inner.clone();

            // Start forwarding task
            tokio::spawn(async move {
                tracing::debug!("[LOOP] TransportClient event forwarding task started");

                while let Ok(transport_event) = transport_events.recv().await {
                    tracing::debug!("[RECV] TransportClient received Transport event");

                    // [TARGET] Special handling of Request packets in MessageReceived
                    match &transport_event {
                        crate::event::TransportEvent::MessageReceived(packet)
                            if packet.header.packet_type == crate::packet::PacketType::Request =>
                        {
                            // Create TransportContext with real response functionality for Request packets
                            let transport = transport_for_response.clone();
                            let message_id = packet.header.message_id;

                            let context = crate::event::TransportContext::new_request(
                                None,
                                message_id,
                                packet.header.biz_type,
                                if packet.ext_header.is_empty() {
                                    None
                                } else {
                                    Some(packet.ext_header.clone())
                                },
                                packet.payload.clone(),
                                Arc::new(move |response_data: Vec<u8>| {
                                    let transport = transport.clone();
                                    tokio::spawn(async move {
                                        // [TARGET] Create response packet
                                        let response_packet = crate::packet::Packet {
                                            header: crate::packet::FixedHeader {
                                                version: 1,
                                                compression: crate::packet::CompressionType::None,
                                                packet_type: crate::packet::PacketType::Response,
                                                biz_type: 0,
                                                message_id,
                                                ext_header_len: 0,
                                                payload_len: response_data.len() as u32,
                                                reserved: crate::packet::ReservedFlags::new(),
                                            },
                                            ext_header: Vec::new(),
                                            payload: response_data,
                                        };

                                        if let Err(e) = transport.send(response_packet).await {
                                            tracing::error!(
                                                "[ERROR] TransportClient response send failed: {}",
                                                e
                                            );
                                        }
                                    });
                                }),
                            );

                            let client_event = crate::event::ClientEvent::MessageReceived(context);
                            tracing::debug!(
                                "[SEND] TransportClient forwarding ClientEvent (Request): {:?}",
                                client_event
                            );

                            if let Err(e) = client_event_sender.send(client_event) {
                                tracing::warn!(
                                    "[WARNING] TransportClient event forwarding failed: {:?}",
                                    e
                                );
                            }
                        }
                        _ => {
                            // Other events use standard conversion
                            if let Some(client_event) =
                                crate::event::ClientEvent::from_transport_event(transport_event)
                            {
                                tracing::debug!(
                                    "[SEND] TransportClient forwarding ClientEvent: {:?}",
                                    client_event
                                );

                                if let Err(e) = client_event_sender.send(client_event) {
                                    tracing::warn!(
                                        "[WARNING] TransportClient event forwarding failed: {:?}",
                                        e
                                    );
                                }
                            } else {
                                tracing::debug!(
                                    "[SKIP] TransportClient skipping unsupported event"
                                );
                            }
                        }
                    }
                }

                tracing::debug!("[END] TransportClient event forwarding task ended");
            });

            tracing::debug!("[SUCCESS] TransportClient event forwarding task started");
            Ok(())
        } else {
            Err(TransportError::connection_error(
                "Connection does not support event streams",
                false,
            ))
        }
    }

    /// [SEND] Send request and wait for response (with options)
    pub async fn request_with_options(
        &self,
        data: Bytes,
        options: super::TransportOptions,
    ) -> Result<Bytes, TransportError> {
        self.inner.request_with_options(data, options).await
    }

    /// [SEND] Send one-way message (with options)
    pub async fn send_with_options(
        &self,
        data: Bytes,
        options: super::TransportOptions,
    ) -> Result<(), TransportError> {
        self.inner.send_with_options(data, options).await
    }
}

// Simplification complete - Unique connection method that meets user requirements

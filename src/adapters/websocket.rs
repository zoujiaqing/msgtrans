use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::{
    accept_async, connect_async,
    tungstenite::{error, protocol::Message, Error as TungsteniteError},
    MaybeTlsStream, WebSocketStream,
};

use crate::{
    command::ConnectionState,
    error::TransportError,
    event::TransportEvent,
    packet::Packet,
    protocol::{AdapterStats, ProtocolAdapter, ProtocolConfig},
    ConnectionInfo, SessionId,
};

/// WebSocket message processing result
enum MessageProcessResult {
    /// Received data packet
    Packet(Packet),
    /// Heartbeat message, continue processing
    Heartbeat,
    /// Peer closed normally
    PeerClosed,
    /// Processing error
    Error(WebSocketError),
}

#[derive(Debug, thiserror::Error)]
pub enum WebSocketError {
    #[error("Tungstenite error: {0}")]
    Tungstenite(#[from] TungsteniteError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Invalid message type")]
    InvalidMessageType,

    #[error("Configuration error: {0}")]
    Config(String),
}

impl From<WebSocketError> for TransportError {
    fn from(error: WebSocketError) -> Self {
        match error {
            WebSocketError::Tungstenite(e) => {
                TransportError::connection_error(format!("WebSocket protocol error: {}", e), true)
            }
            WebSocketError::Io(e) => {
                TransportError::connection_error(format!("WebSocket IO error: {}", e), true)
            }
            WebSocketError::ConnectionClosed => {
                TransportError::connection_error("WebSocket connection closed", false)
            }
            WebSocketError::InvalidMessageType => {
                TransportError::protocol_error("websocket", "Invalid message type")
            }
            WebSocketError::Config(msg) => TransportError::config_error("websocket", msg),
        }
    }
}

/// WebSocket protocol adapter - event-driven version
pub struct WebSocketAdapter<C> {
    /// Session ID (using atomic type for event loop access)
    session_id: Arc<std::sync::atomic::AtomicU64>,
    /// Configuration
    config: C,
    /// Statistics information
    stats: AdapterStats,
    /// Connection information
    connection_info: ConnectionInfo,
    /// Send queue
    send_queue: mpsc::UnboundedSender<Packet>,
    /// Event sender
    event_sender: broadcast::Sender<TransportEvent>,
    /// Shutdown signal sender
    shutdown_sender: mpsc::UnboundedSender<()>,
    /// Event loop handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Connection status
    is_connected: Arc<std::sync::atomic::AtomicBool>,
}

impl<C> WebSocketAdapter<C> {
    pub fn new(config: C) -> Self {
        let (event_sender, _) = broadcast::channel(8192);
        let (send_queue_tx, _) = mpsc::unbounded_channel();
        let (shutdown_tx, _) = mpsc::unbounded_channel();

        Self {
            session_id: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            config,
            stats: AdapterStats::new(),
            connection_info: ConnectionInfo::default(),
            send_queue: send_queue_tx,
            event_sender,
            shutdown_sender: shutdown_tx,
            event_loop_handle: None,
            is_connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create adapter with WebSocket stream
    pub async fn new_with_stream(
        config: C,
        stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
        event_sender: broadcast::Sender<TransportEvent>,
    ) -> Result<Self, WebSocketError> {
        let mut connection_info = ConnectionInfo::default();
        connection_info.protocol = "websocket".to_string();
        connection_info.state = ConnectionState::Connected;
        connection_info.established_at = std::time::SystemTime::now();

        let session_id = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let is_connected = Arc::new(std::sync::atomic::AtomicBool::new(true));

        // Create communication channels
        let (send_queue_tx, send_queue_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();

        // Start event loop
        let event_loop_handle = Self::start_event_loop(
            stream,
            session_id.clone(),
            is_connected.clone(),
            send_queue_rx,
            shutdown_rx,
            event_sender.clone(),
        )
        .await;

        Ok(Self {
            session_id,
            config,
            stats: AdapterStats::new(),
            connection_info,
            send_queue: send_queue_tx,
            event_sender,
            shutdown_sender: shutdown_tx,
            event_loop_handle: Some(event_loop_handle),
            is_connected,
        })
    }

    /// Get event stream receiver
    pub fn subscribe_events(&self) -> broadcast::Receiver<TransportEvent> {
        self.event_sender.subscribe()
    }

    /// Start event loop based on tokio::select!
    async fn start_event_loop(
        mut stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
        session_id: Arc<std::sync::atomic::AtomicU64>,
        is_connected: Arc<std::sync::atomic::AtomicBool>,
        mut send_queue: mpsc::UnboundedReceiver<Packet>,
        mut shutdown_signal: mpsc::UnboundedReceiver<()>,
        event_sender: broadcast::Sender<TransportEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let current_session_id =
                SessionId(session_id.load(std::sync::atomic::Ordering::SeqCst));
            tracing::debug!(
                "[START] WebSocket event loop started (session: {})",
                current_session_id
            );

            loop {
                // Get current session ID
                let current_session_id =
                    SessionId(session_id.load(std::sync::atomic::Ordering::SeqCst));

                tokio::select! {
                    // [RECV] Handle incoming data
                    read_result = stream.next() => {
                        match read_result {
                            Some(Ok(message)) => {
                                match Self::process_websocket_message(message) {
                                    MessageProcessResult::Packet(packet) => {
                                        tracing::debug!("[RECV] WebSocket received packet: {} bytes (session: {})", packet.payload.len(), current_session_id);

                                        // Send receive event
                                        let event = TransportEvent::MessageReceived(packet);

                                        if let Err(e) = event_sender.send(event) {
                                            tracing::warn!("[RECV] Failed to send receive event: {:?}", e);
                                        }
                                    }
                                    MessageProcessResult::Heartbeat => {
                                        // Heartbeat message, continue loop
                                        continue;
                                    }
                                    MessageProcessResult::PeerClosed => {
                                        // Peer closed normally: notify upper layer application that connection is closed for resource cleanup
                                        let close_event = TransportEvent::ConnectionClosed { reason: crate::error::CloseReason::Normal };

                                        if let Err(e) = event_sender.send(close_event) {
                                            tracing::debug!("[CLOSE] Failed to notify upper layer connection closed: session {} - {:?}", current_session_id, e);
                                        } else {
                                            tracing::debug!("[CLOSE] Notified upper layer connection closed: session {}", current_session_id);
                                        }
                                        is_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                                        break;
                                    }
                                    MessageProcessResult::Error(e) => {
                                        tracing::error!("[ERROR] WebSocket message processing error: {:?} (session: {})", e, current_session_id);
                                        // Message processing error: notify upper layer application of connection error for resource cleanup
                                        let close_event = TransportEvent::ConnectionClosed { reason: crate::error::CloseReason::Error(format!("{:?}", e)) };

                                        if let Err(e) = event_sender.send(close_event) {
                                            tracing::debug!("[ERROR] Failed to notify upper layer message processing error: session {} - {:?}", current_session_id, e);
                                        } else {
                                            tracing::debug!("[ERROR] Notified upper layer message processing error: session {}", current_session_id);
                                        }
                                        is_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                                        break;
                                    }
                                }
                            }
                            Some(Err(e)) => {
                                // Gracefully handle different types of WebSocket errors
                                let reason = match e {
                                    TungsteniteError::Protocol(error::ProtocolError::ResetWithoutClosingHandshake) => {
                                        tracing::debug!("[CLOSE] Peer actively reset WebSocket connection (session: {})", current_session_id);
                                        crate::error::CloseReason::Normal
                                    }
                                    TungsteniteError::ConnectionClosed => {
                                        tracing::debug!("[CLOSE] Peer actively closed WebSocket connection (session: {})", current_session_id);
                                        crate::error::CloseReason::Normal
                                    }
                                    _ => {
                                        tracing::error!("[ERROR] WebSocket connection error: {:?} (session: {})", e, current_session_id);
                                        crate::error::CloseReason::Error(format!("{:?}", e))
                                    }
                                };

                                // Network exception or peer closed: notify upper layer application that connection is closed for resource cleanup
                                let close_event = TransportEvent::ConnectionClosed { reason };

                                if let Err(e) = event_sender.send(close_event) {
                                    tracing::debug!("[CLOSE] Failed to notify upper layer connection closed: session {} - {:?}", current_session_id, e);
                                } else {
                                    tracing::debug!("[CLOSE] Notified upper layer connection closed: session {}", current_session_id);
                                }
                                is_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                                break;
                            }
                            None => {
                                tracing::debug!("[CLOSE] Peer actively closed WebSocket connection (session: {})", current_session_id);
                                // Peer actively closed: notify upper layer application that connection is closed for resource cleanup
                                let close_event = TransportEvent::ConnectionClosed { reason: crate::error::CloseReason::Normal };

                                if let Err(e) = event_sender.send(close_event) {
                                    tracing::debug!("[CLOSE] Failed to notify upper layer connection closed: session {} - {:?}", current_session_id, e);
                                } else {
                                    tracing::debug!("[CLOSE] Notified upper layer connection closed: session {}", current_session_id);
                                }
                                is_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                                break;
                            }
                        }
                    }

                    // [SEND] Handle outgoing data - zero-copy optimization
                    packet = send_queue.recv() => {
                        if let Some(packet) = packet {
                            let serialized_data = packet.to_bytes();
                            // [PERF] Optimization: use into() conversion to avoid extra copy (if possible)
                            let message = Message::Binary(serialized_data.into());

                            match stream.send(message).await {
                                Ok(_) => {
                                    tracing::debug!("[SEND] WebSocket send successful: {} bytes (session: {})", packet.payload.len(), current_session_id);

                                    // Send send event
                                    let event = TransportEvent::MessageSent { packet_id: packet.header.message_id };

                                    if let Err(e) = event_sender.send(event) {
                                        tracing::warn!("[SEND] Failed to send send event: {:?}", e);
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("[ERROR] WebSocket send error: {:?} (session: {})", e, current_session_id);
                                    // Send error: notify upper layer application of connection error for resource cleanup
                                    let close_event = TransportEvent::ConnectionClosed { reason: crate::error::CloseReason::Error(format!("{:?}", e)) };

                                    if let Err(e) = event_sender.send(close_event) {
                                        tracing::debug!("[ERROR] Failed to notify upper layer send error: session {} - {:?}", current_session_id, e);
                                    } else {
                                        tracing::debug!("[ERROR] Notified upper layer send error: session {}", current_session_id);
                                    }
                                    is_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                                    break;
                                }
                            }
                        }
                    }

                    // [STOP] Handle shutdown signal
                    _ = shutdown_signal.recv() => {
                        tracing::info!("[STOP] Received shutdown signal, stopping WebSocket event loop (session: {})", current_session_id);
                        // Active close: first send WebSocket Close frame, then close connection
                        tracing::debug!("[CLOSE] Send WebSocket Close frame for graceful shutdown");

                        // Send Close frame
                        if let Err(e) = stream.close(None).await {
                            tracing::warn!("[SEND] Failed to send WebSocket Close frame: {:?} (session: {})", e, current_session_id);
                        } else {
                            tracing::debug!("[SEND] WebSocket Close frame sent successfully (session: {})", current_session_id);
                        }

                        // Active close: no need to send close event, because it was initiated by upper layer
                        // Lower layer protocol close has already notified peer, upper layer already knows about the close
                        tracing::debug!("[CLOSE] Active close, not sending close event");
                        break;
                    }
                }
            }

            tracing::debug!(
                "[SUCCESS] WebSocket event loop ended (session: {})",
                current_session_id
            );
        })
    }

    /// Process WebSocket message - optimized version
    fn process_websocket_message(message: Message) -> MessageProcessResult {
        match message {
            Message::Binary(data) => {
                // [PERF] Optimization: WebSocket guarantees message integrity, can parse directly
                // Pre-check minimum length to avoid unnecessary parsing attempts
                if data.len() < 16 {
                    // Data too short, cannot be valid Packet, create basic data packet directly
                    let packet = Packet::one_way(0, data.clone());
                    return MessageProcessResult::Packet(packet);
                }

                // Try to parse as complete Packet
                match Packet::from_bytes(&data) {
                    Ok(packet) => {
                        tracing::debug!(
                            "[RECV] WebSocket packet parsing successful: {} bytes",
                            packet.payload.len()
                        );
                        MessageProcessResult::Packet(packet)
                    }
                    Err(e) => {
                        tracing::debug!("[RECV] WebSocket packet parsing failed: {:?}, creating basic data packet", e);
                        // If parsing fails, create a basic data packet
                        let packet = Packet::one_way(0, data.clone());
                        MessageProcessResult::Packet(packet)
                    }
                }
            }
            Message::Text(text) => {
                // [SUCCESS] Text message creates data packet directly (usually for debugging)
                tracing::debug!(
                    "[RECV] WebSocket received text message: {} bytes",
                    text.len()
                );
                let packet = Packet::one_way(0, text.as_bytes());
                MessageProcessResult::Packet(packet)
            }
            Message::Close(_) => {
                // Close message indicates peer closed normally
                tracing::debug!("[RECV] WebSocket received Close message");
                MessageProcessResult::PeerClosed
            }
            Message::Ping(_) | Message::Pong(_) => {
                // Heartbeat message, handle silently
                MessageProcessResult::Heartbeat
            }
            Message::Frame(_) => {
                tracing::warn!("[RECV] WebSocket received unsupported Frame message");
                MessageProcessResult::Error(WebSocketError::InvalidMessageType)
            }
        }
    }
}

#[async_trait]
impl<C> ProtocolAdapter for WebSocketAdapter<C>
where
    C: Send + Sync + 'static + ProtocolConfig,
{
    type Config = C;
    type Error = WebSocketError;

    async fn send(&mut self, packet: Packet) -> Result<(), Self::Error> {
        // Use send queue instead of direct sending
        self.send_queue
            .send(packet)
            .map_err(|_| WebSocketError::ConnectionClosed)?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let current_session_id =
            SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst));
        tracing::debug!(
            "[CLOSE] Close WebSocket connection (session: {})",
            current_session_id
        );

        // Send shutdown signal
        let _ = self.shutdown_sender.send(());

        // Wait for event loop to end
        if let Some(handle) = self.event_loop_handle.take() {
            let _ = handle.await;
        }

        self.is_connected
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn connection_info(&self) -> ConnectionInfo {
        self.connection_info.clone()
    }

    fn is_connected(&self) -> bool {
        self.is_connected.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn stats(&self) -> AdapterStats {
        self.stats.clone()
    }

    fn session_id(&self) -> SessionId {
        SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst))
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        self.session_id
            .store(session_id.0, std::sync::atomic::Ordering::SeqCst);
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) struct WebSocketServerBuilder<C> {
    config: Option<C>,
}

impl<C> WebSocketServerBuilder<C> {
    pub(crate) fn new() -> Self {
        Self { config: None }
    }

    pub(crate) fn config(mut self, config: C) -> Self {
        self.config = Some(config);
        self
    }

    pub(crate) fn bind_address(self, _addr: std::net::SocketAddr) -> Self {
        self
    }

    pub(crate) async fn build(self) -> Result<WebSocketServer<C>, WebSocketError> {
        let config = self
            .config
            .ok_or_else(|| WebSocketError::Config("Missing WebSocket server config".to_string()))?;
        Ok(WebSocketServer {
            config,
            listener: None,
        })
    }
}

pub(crate) struct WebSocketServer<C> {
    config: C,
    listener: Option<TcpListener>,
}

impl<C: 'static> WebSocketServer<C> {
    pub(crate) async fn accept(&mut self) -> Result<WebSocketAdapter<C>, WebSocketError>
    where
        C: Clone + crate::protocol::ProtocolConfig,
    {
        // Create listener if not already created
        if self.listener.is_none() {
            let bind_addr = if let Some(ws_config) = (&self.config as &dyn std::any::Any)
                .downcast_ref::<crate::protocol::WebSocketServerConfig>(
            ) {
                ws_config.bind_address.to_string()
            } else {
                "127.0.0.1:8080".parse().unwrap()
            };

            let listener = TcpListener::bind(&bind_addr).await?;
            tracing::debug!("[START] WebSocket server listening on: {}", bind_addr);
            self.listener = Some(listener);
        }

        if let Some(listener) = &self.listener {
            let (tcp_stream, addr) = listener.accept().await?;
            tracing::debug!("[ACCEPT] WebSocket server accepted connection: {}", addr);

            // Perform WebSocket handshake
            let maybe_tls_stream = MaybeTlsStream::Plain(tcp_stream);
            let ws_stream = accept_async(maybe_tls_stream).await?;

            // Create event sender
            let (event_sender, _) = broadcast::channel(8192);

            // Create WebSocket adapter
            WebSocketAdapter::new_with_stream(self.config.clone(), ws_stream, event_sender).await
        } else {
            Err(WebSocketError::Config("No listener available".to_string()))
        }
    }

    pub(crate) fn local_addr(&self) -> Result<std::net::SocketAddr, WebSocketError> {
        if let Some(listener) = &self.listener {
            listener.local_addr().map_err(WebSocketError::Io)
        } else {
            Err(WebSocketError::Config("Server not bound".to_string()))
        }
    }
}

pub(crate) struct WebSocketClientBuilder<C> {
    config: Option<C>,
}

impl<C> WebSocketClientBuilder<C> {
    pub(crate) fn new() -> Self {
        Self { config: None }
    }

    pub(crate) fn config(mut self, config: C) -> Self {
        self.config = Some(config);
        self
    }

    pub(crate) fn target_url<S: Into<String>>(self, _url: S) -> Self {
        self
    }

    pub(crate) async fn connect(self) -> Result<WebSocketAdapter<C>, WebSocketError>
    where
        C: crate::protocol::ProtocolConfig,
    {
        let config = self
            .config
            .ok_or_else(|| WebSocketError::Config("Missing WebSocket client config".to_string()))?;

        // Get connection URL from configuration
        let url = if let Some(ws_config) =
            (&config as &dyn std::any::Any).downcast_ref::<crate::protocol::WebSocketClientConfig>()
        {
            ws_config.target_url.clone()
        } else {
            "ws://127.0.0.1:8080".to_string()
        };

        tracing::debug!("[CONNECT] WebSocket client connecting to: {}", url);

        // Connect to WebSocket server
        let (ws_stream, _) = connect_async(&url).await?;

        tracing::debug!("[SUCCESS] WebSocket client connected to: {}", url);

        // Create event sender
        let (event_sender, _) = broadcast::channel(8192);

        // Create WebSocket adapter
        WebSocketAdapter::new_with_stream(config, ws_stream, event_sender).await
    }
}

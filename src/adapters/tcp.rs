use crate::{
    command::{ConnectionInfo, ConnectionState},
    error::TransportError,
    event::TransportEvent,
    packet::{Packet, PacketError},
    protocol::{AdapterStats, ProtocolAdapter, TcpClientConfig, TcpServerConfig},
    SessionId,
};
use async_trait::async_trait;
use bytes::BytesMut;
use std::io;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc},
};

/// TCP adapter error types
#[derive(Debug, thiserror::Error)]
pub enum TcpError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Connection timeout")]
    Timeout,

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Packet error: {0}")]
    Packet(#[from] PacketError),

    #[error("Buffer overflow")]
    BufferOverflow,

    #[error("Configuration error: {0}")]
    Config(String),
}

impl From<TcpError> for TransportError {
    fn from(error: TcpError) -> Self {
        match error {
            TcpError::Io(io_err) => {
                TransportError::connection_error(format!("TCP IO error: {:?}", io_err), true)
            }
            TcpError::Timeout => TransportError::connection_error("TCP connection timeout", true),
            TcpError::ConnectionClosed => {
                TransportError::connection_error("TCP connection closed", true)
            }
            TcpError::Packet(packet_err) => TransportError::protocol_error(
                "packet",
                format!("TCP packet error: {}", packet_err),
            ),
            TcpError::BufferOverflow => {
                TransportError::protocol_error("generic", "TCP buffer overflow".to_string())
            }
            TcpError::Config(msg) => TransportError::config_error("tcp", msg),
        }
    }
}

/// Maximum payload size (1 MB)
const MAX_PAYLOAD_SIZE: usize = 1024 * 1024;
/// Maximum extended header size (64 KB)
const MAX_EXT_HEADER_SIZE: usize = 64 * 1024;
/// Maximum scan distance for frame resync (4 KB)
const MAX_RESYNC_SCAN_DISTANCE: usize = 4096;
/// Fixed header size
const FIXED_HEADER_SIZE: usize = 16;

/// Optimized TCP read buffer with frame resync capability
///
/// Features:
/// 1. Zero-copy packet parsing
/// 2. Streaming read buffering
/// 3. Memory pool reuse
/// 4. Enhanced header validation
/// 5. Frame resync on corruption detection
#[derive(Debug)]
struct OptimizedReadBuffer {
    /// Main read buffer
    buffer: BytesMut,
    /// Target buffer size
    target_capacity: usize,
    /// Statistics
    stats: ReadBufferStats,
}

#[derive(Debug, Default)]
struct ReadBufferStats {
    /// Number of reads
    reads: u64,
    /// Number of parsed packets
    packets_parsed: u64,
    /// Number of buffer reallocations
    reallocations: u64,
    /// Total bytes read
    bytes_read: u64,
    /// Number of resync attempts
    resync_attempts: u64,
    /// Number of bytes discarded during resync
    bytes_discarded: u64,
}

impl OptimizedReadBuffer {
    /// Create new read buffer
    fn new(initial_capacity: usize) -> Self {
        Self {
            buffer: BytesMut::with_capacity(initial_capacity),
            target_capacity: initial_capacity,
            stats: ReadBufferStats::default(),
        }
    }

    /// Validate header fields at given offset
    ///
    /// Checks:
    /// - version must be 1
    /// - compression must be 0-2
    /// - packet_type must be 0-2
    /// - payload_len must be <= MAX_PAYLOAD_SIZE
    /// - ext_header_len must be <= MAX_EXT_HEADER_SIZE
    fn is_valid_header_at(&self, offset: usize) -> bool {
        if self.buffer.len() < offset + FIXED_HEADER_SIZE {
            return false;
        }

        let header = &self.buffer[offset..offset + FIXED_HEADER_SIZE];

        // Validate version (must be 1)
        let version = header[0];
        if version != 1 {
            return false;
        }

        // Validate compression type (0=None, 1=Zstd, 2=Zlib)
        let compression = header[1];
        if compression > 2 {
            return false;
        }

        // Validate packet type (0=OneWay, 1=Request, 2=Response)
        let packet_type = header[2];
        if packet_type > 2 {
            return false;
        }

        // biz_type (header[3]) can be any value 0-255, no validation needed

        // Validate ext_header_len
        let ext_header_len = u16::from_be_bytes([header[8], header[9]]) as usize;
        if ext_header_len > MAX_EXT_HEADER_SIZE {
            return false;
        }

        // Validate payload_len
        let payload_len =
            u32::from_be_bytes([header[10], header[11], header[12], header[13]]) as usize;
        if payload_len > MAX_PAYLOAD_SIZE {
            return false;
        }

        true
    }

    /// Attempt to resync frame boundary after detecting corruption
    ///
    /// Scans forward byte-by-byte looking for a valid header.
    /// Returns true if resync successful, false if should disconnect.
    fn try_resync_frame(&mut self) -> bool {
        self.stats.resync_attempts += 1;

        let scan_limit = self.buffer.len().min(MAX_RESYNC_SCAN_DISTANCE);

        for offset in 1..scan_limit {
            if self.is_valid_header_at(offset) {
                // Found valid header, discard corrupted bytes
                tracing::warn!(
                    "[RESYNC] Frame resync successful, discarded {} bytes",
                    offset
                );
                self.stats.bytes_discarded += offset as u64;
                let _ = self.buffer.split_to(offset);
                return true;
            }
        }

        // No valid header found within scan limit
        if self.buffer.len() > MAX_RESYNC_SCAN_DISTANCE {
            // Discard scanned bytes and continue
            tracing::warn!(
                "[RESYNC] No valid frame found in {} bytes, discarding",
                MAX_RESYNC_SCAN_DISTANCE
            );
            self.stats.bytes_discarded += MAX_RESYNC_SCAN_DISTANCE as u64;
            let _ = self.buffer.split_to(MAX_RESYNC_SCAN_DISTANCE);
            return true;
        }

        // Buffer too small, need more data
        true
    }

    /// Try to parse next complete packet from buffer
    ///
    /// Returns:
    /// - Ok(Some(packet)) - Successfully parsed a complete packet
    /// - Ok(None) - No complete packet in buffer (need more data)
    /// - Err(error) - Unrecoverable parse error
    fn try_parse_next_packet(&mut self) -> Result<Option<Packet>, TcpError> {
        loop {
            // Check if there's enough data for header
            if self.buffer.len() < FIXED_HEADER_SIZE {
                return Ok(None);
            }

            // Validate header at current position
            if !self.is_valid_header_at(0) {
                tracing::debug!("[PARSE] Invalid header detected, attempting resync");
                if !self.try_resync_frame() {
                    return Err(TcpError::BufferOverflow);
                }
                // Continue loop to try parsing again
                continue;
            }

            // Header is valid, extract lengths
            let header_bytes = &self.buffer[0..FIXED_HEADER_SIZE];
            let ext_header_len = u16::from_be_bytes([header_bytes[8], header_bytes[9]]) as usize;
            let payload_len = u32::from_be_bytes([
                header_bytes[10],
                header_bytes[11],
                header_bytes[12],
                header_bytes[13],
            ]) as usize;

            let total_packet_len = FIXED_HEADER_SIZE + ext_header_len + payload_len;

            // Check if complete packet is available
            if self.buffer.len() < total_packet_len {
                return Ok(None);
            }

            // Zero-copy parsing: split packet from buffer
            let packet_bytes = self.buffer.split_to(total_packet_len).freeze();

            // Parse packet
            match Packet::from_bytes(&packet_bytes) {
                Ok(packet) => {
                    self.stats.packets_parsed += 1;
                    return Ok(Some(packet));
                }
                Err(e) => {
                    // Packet parsing failed, try resync
                    tracing::warn!("[PARSE] Packet parse error: {:?}, attempting resync", e);
                    // Put bytes back? No, they're already split. Try resync on remaining.
                    if !self.try_resync_frame() {
                        return Err(TcpError::Packet(e));
                    }
                    continue;
                }
            }
        }
    }

    /// Read more data from stream to buffer
    async fn fill_from_stream(
        &mut self,
        read_half: &mut tokio::net::tcp::OwnedReadHalf,
    ) -> Result<usize, TcpError> {
        // Ensure buffer has enough space
        if self.buffer.capacity() - self.buffer.len() < 4096 {
            self.buffer.reserve(self.target_capacity);
            self.stats.reallocations += 1;
        }

        // Read data
        let bytes_read = read_half
            .read_buf(&mut self.buffer)
            .await
            .map_err(TcpError::Io)?;

        self.stats.reads += 1;
        self.stats.bytes_read += bytes_read as u64;

        Ok(bytes_read)
    }

    /// Get buffer statistics
    fn stats(&self) -> &ReadBufferStats {
        &self.stats
    }

    /// Clear buffer (preserving capacity)
    fn clear(&mut self) {
        self.buffer.clear();
    }
}

/// TCP protocol adapter - event-driven version
pub struct TcpAdapter<C> {
    /// Session ID (using atomic type for event loop access)
    session_id: Arc<std::sync::atomic::AtomicU64>,
    /// Configuration
    config: C,
    /// Statistics
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
}

impl<C> TcpAdapter<C> {
    /// Create new TCP adapter
    pub async fn new(
        stream: TcpStream,
        config: C,
        event_sender: broadcast::Sender<TransportEvent>,
    ) -> Result<Self, TcpError> {
        // Set basic TCP options
        stream.set_nodelay(true)?;

        let local_addr = stream.local_addr()?;
        let peer_addr = stream.peer_addr()?;

        let mut connection_info = ConnectionInfo::default();
        connection_info.local_addr = local_addr;
        connection_info.peer_addr = peer_addr;
        connection_info.protocol = "tcp".to_string();
        connection_info.state = ConnectionState::Connected;
        connection_info.established_at = std::time::SystemTime::now();

        let session_id = Arc::new(std::sync::atomic::AtomicU64::new(0)); // Temporary ID, will be set later

        // Create communication channels
        let (send_queue_tx, send_queue_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();

        // Start event loop
        let event_loop_handle = Self::start_event_loop(
            stream,
            session_id.clone(),
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
        })
    }

    /// Get event stream receiver
    ///
    /// This allows clients to subscribe to events sent by TCP adapter internal event loop
    pub fn subscribe_events(&self) -> broadcast::Receiver<TransportEvent> {
        self.event_sender.subscribe()
    }

    /// Start tokio::select! based event loop
    async fn start_event_loop(
        stream: TcpStream,
        session_id: Arc<std::sync::atomic::AtomicU64>,
        mut send_queue: mpsc::UnboundedReceiver<Packet>,
        mut shutdown_signal: mpsc::UnboundedReceiver<()>,
        event_sender: broadcast::Sender<TransportEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let current_session_id =
                SessionId(session_id.load(std::sync::atomic::Ordering::SeqCst));
            tracing::debug!(
                "[START] TCP event loop started (session: {})",
                current_session_id
            );

            // Split read/write streams
            let (mut read_half, mut write_half) = stream.into_split();

            loop {
                // Get current session ID
                let current_session_id =
                    SessionId(session_id.load(std::sync::atomic::Ordering::SeqCst));

                tokio::select! {
                    // [RECV] Handle receive data - using optimized buffer method
                    read_result = async {
                        let mut temp_buffer = OptimizedReadBuffer::new(8192);
                        match temp_buffer.fill_from_stream(&mut read_half).await {
                            Ok(0) => Ok(None), // Connection closed
                            Ok(_) => {
                                // Try to parse packet
                                temp_buffer.try_parse_next_packet()
                            }
                            Err(e) => Err(e),
                        }
                    } => {
                        match read_result {
                            Ok(Some(packet)) => {
                                tracing::debug!("[RECV] TCP received packet: {} bytes (session: {})", packet.payload.len(), current_session_id);
                                tracing::debug!("[DETAIL] Packet details: ID={}, type={:?}, payload_len={}", packet.header.message_id, packet.header.packet_type, packet.payload.len());

                                // Send receive event
                                let event = TransportEvent::MessageReceived(packet);

                                if let Err(e) = event_sender.send(event) {
                                    tracing::warn!("[RECV] Failed to send receive event: {:?}", e);
                                }
                            }
                            Ok(None) => {
                                tracing::debug!("[RECV] Peer actively closed TCP connection (session: {})", current_session_id);
                                // Peer actively closed: notify upper layer that connection is closed for resource cleanup
                                let close_event = TransportEvent::ConnectionClosed { reason: crate::error::CloseReason::Normal };

                                if let Err(e) = event_sender.send(close_event) {
                                    tracing::debug!("[CONNECT] Failed to notify upper layer of connection close: session {} - {:?}", current_session_id, e);
                                } else {
                                    tracing::debug!("[NOTIFY] Notified upper layer of connection close: session {}", current_session_id);
                                }
                                break;
                            }
                            Err(e) => {
                                tracing::error!("[RECV] TCP connection error: {:?} (session: {})", e, current_session_id);
                                // Network error: notify upper layer of connection error for resource cleanup
                                let close_event = TransportEvent::ConnectionClosed { reason: crate::error::CloseReason::Error(format!("{:?}", e)) };

                                if let Err(e) = event_sender.send(close_event) {
                                    tracing::debug!("[CONNECT] Failed to notify upper layer of connection error: session {} - {:?}", current_session_id, e);
                                } else {
                                    tracing::debug!("[NOTIFY] Notified upper layer of connection error: session {}", current_session_id);
                                }
                                break;
                            }
                        }
                    }

                    // [SEND] Handle send data
                    packet = send_queue.recv() => {
                        if let Some(packet) = packet {
                            match Self::write_packet_to_stream(&mut write_half, &packet).await {
                                Ok(_) => {
                                    tracing::debug!("[SEND] TCP send successful: {} bytes (session: {})", packet.payload.len(), current_session_id);

                                    // Send send event
                                    let event = TransportEvent::MessageSent { packet_id: packet.header.message_id };

                                    if let Err(e) = event_sender.send(event) {
                                        tracing::warn!("[SEND] Failed to send send event: {:?}", e);
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("[SEND] TCP send error: {:?} (session: {})", e, current_session_id);
                                    // Send error: notify upper layer of connection error for resource cleanup
                                    let close_event = TransportEvent::ConnectionClosed { reason: crate::error::CloseReason::Error(format!("{:?}", e)) };

                                    if let Err(e) = event_sender.send(close_event) {
                                        tracing::debug!("[CONNECT] Failed to notify upper layer of send error: session {} - {:?}", current_session_id, e);
                                    } else {
                                        tracing::debug!("[NOTIFY] Notified upper layer of send error: session {}", current_session_id);
                                    }
                                    break;
                                }
                            }
                        }
                    }

                    // [STOP] Handle shutdown signal
                    _ = shutdown_signal.recv() => {
                        tracing::info!("[STOP] Received shutdown signal, stopping TCP event loop (session: {})", current_session_id);
                        // Active close: no need to send close event, as upper layer initiated the close
                        // Lower layer protocol close already notified peer, upper layer also knows about the close
                        tracing::debug!("[CLOSE] Active close, not sending close event");
                        break;
                    }
                }
            }

            tracing::debug!(
                "[SUCCESS] TCP event loop ended (session: {})",
                current_session_id
            );
        })
    }

    /// Write packet to stream
    async fn write_packet_to_stream(
        write_half: &mut tokio::net::tcp::OwnedWriteHalf,
        packet: &Packet,
    ) -> Result<(), TcpError> {
        let packet_bytes = packet.to_bytes();
        write_half
            .write_all(&packet_bytes)
            .await
            .map_err(TcpError::Io)?;
        write_half.flush().await.map_err(TcpError::Io)?;
        Ok(())
    }
}

// Client adapter implementation
impl TcpAdapter<TcpClientConfig> {
    /// Connect to TCP server
    pub async fn connect(
        addr: std::net::SocketAddr,
        config: TcpClientConfig,
    ) -> Result<Self, TcpError> {
        tracing::debug!("[CONNECT] TCP client connecting to: {}", addr);

        let stream = if config.connect_timeout != std::time::Duration::from_secs(0) {
            tokio::time::timeout(config.connect_timeout, TcpStream::connect(addr))
                .await
                .map_err(|_| TcpError::Timeout)?
                .map_err(TcpError::Io)?
        } else {
            TcpStream::connect(addr).await.map_err(TcpError::Io)?
        };

        tracing::debug!("[SUCCESS] TCP connection established successfully");

        Self::new(stream, config, broadcast::channel(8192).0).await
    }
}

#[async_trait]
impl ProtocolAdapter for TcpAdapter<TcpClientConfig> {
    type Config = TcpClientConfig;
    type Error = TcpError;

    async fn send(&mut self, packet: Packet) -> Result<(), Self::Error> {
        let current_session_id =
            SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst));
        tracing::debug!(
            "[SEND] TCP sending packet: {} bytes (session: {})",
            packet.payload.len(),
            current_session_id
        );

        // Send packet through queue, event loop will handle actual sending
        self.send_queue
            .send(packet)
            .map_err(|_| TcpError::ConnectionClosed)?;

        Ok(())
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let current_session_id =
            SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst));
        tracing::debug!(
            "[CLOSE] Closing TCP connection (session: {})",
            current_session_id
        );

        // Send shutdown signal
        let _ = self.shutdown_sender.send(());

        // Wait for event loop to end
        if let Some(handle) = self.event_loop_handle.take() {
            let _ = handle.await;
        }

        self.connection_info.state = ConnectionState::Closed;
        self.connection_info.closed_at = Some(std::time::SystemTime::now());

        Ok(())
    }

    fn connection_info(&self) -> ConnectionInfo {
        self.connection_info.clone()
    }

    fn is_connected(&self) -> bool {
        self.connection_info.state == ConnectionState::Connected
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
        self.connection_info.session_id = session_id;
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // In event-driven mode, flush is handled automatically by the event loop
        Ok(())
    }
}

// Server adapter implementation
#[async_trait]
impl ProtocolAdapter for TcpAdapter<TcpServerConfig> {
    type Config = TcpServerConfig;
    type Error = TcpError;

    async fn send(&mut self, packet: Packet) -> Result<(), Self::Error> {
        let current_session_id =
            SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst));
        tracing::debug!(
            "[SEND] TCP sending packet: {} bytes (session: {})",
            packet.payload.len(),
            current_session_id
        );

        // Send packet through queue, event loop will handle actual sending
        self.send_queue
            .send(packet)
            .map_err(|_| TcpError::ConnectionClosed)?;

        Ok(())
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let current_session_id =
            SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst));
        tracing::debug!(
            "[CLOSE] Closing TCP connection (session: {})",
            current_session_id
        );

        // Send shutdown signal
        let _ = self.shutdown_sender.send(());

        // Wait for event loop to end
        if let Some(handle) = self.event_loop_handle.take() {
            let _ = handle.await;
        }

        self.connection_info.state = ConnectionState::Closed;
        self.connection_info.closed_at = Some(std::time::SystemTime::now());

        Ok(())
    }

    fn connection_info(&self) -> ConnectionInfo {
        self.connection_info.clone()
    }

    fn is_connected(&self) -> bool {
        self.connection_info.state == ConnectionState::Connected
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
        self.connection_info.session_id = session_id;
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // In event-driven mode, flush is automatically handled by event loop
        Ok(())
    }
}

/// TCP server builder
pub(crate) struct TcpServerBuilder {
    config: TcpServerConfig,
    bind_address: Option<std::net::SocketAddr>,
}

impl TcpServerBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: TcpServerConfig::default(),
            bind_address: None,
        }
    }

    pub(crate) fn bind_address(mut self, addr: std::net::SocketAddr) -> Self {
        self.bind_address = Some(addr);
        self
    }

    pub(crate) fn config(mut self, config: TcpServerConfig) -> Self {
        self.config = config;
        self
    }

    pub(crate) async fn build(self) -> Result<TcpServer, TcpError> {
        let bind_addr = self.bind_address.unwrap_or(self.config.bind_address);

        tracing::debug!("[START] TCP server starting on: {}", bind_addr);

        let listener = TcpListener::bind(bind_addr).await?;

        tracing::info!(
            "[SUCCESS] TCP server successfully started on: {}",
            listener.local_addr()?
        );

        Ok(TcpServer {
            listener,
            config: self.config,
        })
    }
}

impl Default for TcpServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// TCP server
pub(crate) struct TcpServer {
    listener: TcpListener,
    config: TcpServerConfig,
}

impl TcpServer {
    pub(crate) fn builder() -> TcpServerBuilder {
        TcpServerBuilder::new()
    }

    pub(crate) async fn accept(&mut self) -> Result<TcpAdapter<TcpServerConfig>, TcpError> {
        let (stream, peer_addr) = self.listener.accept().await?;

        tracing::debug!("[CONNECT] TCP new connection from: {}", peer_addr);

        TcpAdapter::new(stream, self.config.clone(), broadcast::channel(8192).0).await
    }

    pub(crate) fn local_addr(&self) -> Result<std::net::SocketAddr, TcpError> {
        Ok(self.listener.local_addr()?)
    }
}

/// TCP client builder
pub(crate) struct TcpClientBuilder {
    config: TcpClientConfig,
    target_address: Option<std::net::SocketAddr>,
}

impl TcpClientBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: TcpClientConfig::default(),
            target_address: None,
        }
    }

    pub(crate) fn target_address(mut self, addr: std::net::SocketAddr) -> Self {
        self.target_address = Some(addr);
        self
    }

    pub(crate) fn config(mut self, config: TcpClientConfig) -> Self {
        self.config = config;
        self
    }

    pub(crate) async fn connect(self) -> Result<TcpAdapter<TcpClientConfig>, TcpError> {
        let target_addr = self.target_address.unwrap_or(self.config.target_address);
        TcpAdapter::connect(target_addr, self.config).await
    }
}

impl Default for TcpClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

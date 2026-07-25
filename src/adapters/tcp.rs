use crate::{
    command::{ConnectionInfo, ConnectionState},
    connection::Connection,
    error::TransportError,
    event::TransportEvent,
    packet::{Packet, PacketError},
    protocol::{TcpClientConfig, TcpServerConfig},
    transport::memory_pool::{shared_memory_pool, BufferSize, OptimizedMemoryPool},
    SessionId,
};
use async_trait::async_trait;
use bytes::BytesMut;
use std::io;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

/// Apply TCP keepalive to a TcpStream (cross-platform via socket2::SockRef)
fn apply_tcp_keepalive(stream: &TcpStream, duration: std::time::Duration) {
    let sock_ref = socket2::SockRef::from(&stream);
    let keepalive = socket2::TcpKeepalive::new().with_time(duration);
    if let Err(e) = sock_ref.set_tcp_keepalive(&keepalive) {
        tracing::warn!("Failed to set TCP keepalive: {}", e);
    }
}

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

/// Optimized TCP read buffer with frame resync and memory-pool recycling.
struct OptimizedReadBuffer {
    buffer: BytesMut,
    target_capacity: usize,
    progress: ReadProgress,
    pool: Arc<OptimizedMemoryPool>,
    buffer_tier: BufferSize,
}

/// The only read-loop state that drives a DECISION.
///
/// 2.0 removed the six-counter `ReadBufferStats` (reads / packets_parsed /
/// reallocations / bytes_read / resync_attempts / bytes_discarded): five were
/// write-only — incremented on the hot path for every read, every parse and
/// every resync, and never read by anything — and the sixth was only ever
/// compared against zero. That single question is what remains.
#[derive(Debug, Default)]
struct ReadProgress {
    /// Whether any packet has been parsed on this connection yet. Under the
    /// lenient policy the FIRST invalid header closes the connection (it is
    /// almost certainly non-protocol traffic), while a later one triggers a
    /// bounded resync.
    parsed_any: bool,
}

impl Drop for OptimizedReadBuffer {
    fn drop(&mut self) {
        if self.buffer.capacity() > 0 {
            let mut buf = std::mem::replace(&mut self.buffer, BytesMut::new());
            buf.clear();
            self.pool.return_buffer(buf, self.buffer_tier);
        }
    }
}

impl OptimizedReadBuffer {
    fn new_with_pool(initial_capacity: usize, pool: Arc<OptimizedMemoryPool>) -> Self {
        let buffer_tier = if initial_capacity <= 1024 {
            BufferSize::Small
        } else if initial_capacity <= 8192 {
            BufferSize::Medium
        } else {
            BufferSize::Large
        };
        let buffer = pool.get_buffer(buffer_tier);
        Self {
            buffer,
            target_capacity: initial_capacity,
            progress: ReadProgress::default(),
            pool,
            buffer_tier,
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
        let scan_limit = self.buffer.len().min(MAX_RESYNC_SCAN_DISTANCE);

        for offset in 1..scan_limit {
            if self.is_valid_header_at(offset) {
                // Found valid header, discard corrupted bytes
                tracing::warn!(
                    "[RESYNC] Frame resync successful, discarded {} bytes",
                    offset
                );
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
            let _ = self.buffer.split_to(MAX_RESYNC_SCAN_DISTANCE);
            return true;
        }

        // Buffer too small and no valid header found - signal caller to stop parsing
        // and wait for more data. Returning true here would cause an infinite loop
        // in try_parse_next_packet() because the buffer is not consumed but still
        // >= FIXED_HEADER_SIZE.
        false
    }

    /// The stream decode limits: TCP enforces its own (tighter) caps, shared
    /// with `is_valid_header_at` so the resync scanner and the codec agree on
    /// what a valid frame is.
    fn decode_limits() -> crate::packet::DecodeLimits {
        crate::packet::DecodeLimits {
            max_frame_size: FIXED_HEADER_SIZE + MAX_EXT_HEADER_SIZE + MAX_PAYLOAD_SIZE,
            max_payload_size: MAX_PAYLOAD_SIZE,
            max_ext_header_size: MAX_EXT_HEADER_SIZE,
        }
    }

    /// Try to parse next complete packet from buffer.
    ///
    /// Framing is delegated to the shared codec ([`Packet::decode_one_with`]),
    /// so TCP no longer maintains a second length-parsing implementation.
    ///
    /// Policy:
    /// - `Strict` (default): ANY invalid header closes the connection —
    ///   including after valid packets have been parsed. A byte stream that
    ///   lost sync is not trustworthy.
    /// - `Lenient`: 1.x behavior — invalid first header still fast-fails
    ///   (non-protocol traffic), later corruption attempts a bounded resync.
    ///
    /// Returns:
    /// - Ok(Some(packet)) - Successfully parsed a complete packet
    /// - Ok(None) - No complete packet in buffer (need more data)
    /// - Err(error) - Unrecoverable parse error
    fn try_parse_next_packet(&mut self, strict: bool) -> Result<Option<Packet>, TcpError> {
        let limits = Self::decode_limits();
        loop {
            // Zero-copy: peek the header for the frame length, split that many
            // bytes off the read buffer as an owned Bytes, and slice the body
            // out of it (ref-counted, no payload memcpy).
            match Packet::frame_len(&self.buffer, &limits) {
                Ok(Some(total)) => {
                    let frame = self.buffer.split_to(total).freeze();
                    let packet = Packet::decode_exact_from(&frame, &limits)?;
                    self.progress.parsed_any = true;
                    return Ok(Some(packet));
                }
                Ok(None) => return Ok(None),
                Err(e @ PacketError::FrameTooLarge { .. }) => {
                    // Refused from the header, BEFORE waiting for or buffering
                    // the declared payload, and before consuming any bytes.
                    // Both policies close: resyncing from inside a frame the
                    // peer declared but we refused would only skip past valid
                    // data (the peer is either broken or hostile).
                    tracing::warn!(
                        "[PARSE] Oversized declared frame, closing connection: {:?}",
                        e
                    );
                    return Err(TcpError::Packet(e));
                }
                Err(e) => {
                    // Invalid header (version/type/compression) at the front
                    // of the stream. Nothing has been consumed.
                    if strict {
                        tracing::warn!(
                            "[PARSE] Invalid header under strict policy, closing connection: {:?}",
                            e
                        );
                        return Err(TcpError::Packet(e));
                    }
                    // Lenient: fast-fail for non-protocol traffic on the very
                    // first packet, bounded resync afterwards.
                    if !self.progress.parsed_any {
                        tracing::warn!(
                            "[PARSE] Invalid protocol header on first packet, closing connection"
                        );
                        return Err(TcpError::Config(
                            "Invalid protocol header on first packet".to_string(),
                        ));
                    }
                    tracing::debug!("[PARSE] Invalid header detected, attempting resync");
                    if !self.try_resync_frame() {
                        return Err(TcpError::BufferOverflow);
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
        }

        // Read data
        let bytes_read = read_half
            .read_buf(&mut self.buffer)
            .await
            .map_err(TcpError::Io)?;

        Ok(bytes_read)
    }
}

/// TCP protocol adapter - event-driven version
pub struct TcpAdapter<C> {
    /// Connection liveness + session id, shared with the event loop.
    state: crate::adapters::core::ConnState,
    /// Configuration (retained to keep the generic `C` and for diagnostics).
    #[allow(dead_code)]
    config: C,
    /// Adapter statistics (diagnostics; not on the hot path).
    #[allow(dead_code)]
    /// Connection information
    connection_info: ConnectionInfo,
    /// Send queue
    send_queue: mpsc::Sender<crate::adapters::outbound::Outbound>,
    /// Consumer half of the bounded event pipe, taken exactly once.
    event_pipe_rx: Option<crate::adapters::events::EventPipeRx>,
    /// Shutdown signal sender
    shutdown_sender: mpsc::UnboundedSender<()>,
    /// Event loop handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Frame decode policy, shared with the read loop (Strict by default).
    frame_policy: Arc<std::sync::atomic::AtomicU8>,
}

impl<C> TcpAdapter<C> {
    pub async fn new(
        stream: TcpStream,
        config: C,
        nodelay: bool,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self, TcpError>
    where
        C: std::any::Any,
    {
        // Wired from the protocol config — previously hardcoded to true, which
        // made the nodelay option a lie.
        stream.set_nodelay(nodelay)?;

        let local_addr = stream.local_addr()?;
        let peer_addr = stream.peer_addr()?;

        let mut connection_info = ConnectionInfo::default();
        connection_info.local_addr = local_addr;
        connection_info.peer_addr = peer_addr;
        connection_info.protocol = "tcp".to_string();
        connection_info.state = ConnectionState::Connected;
        connection_info.established_at = std::time::SystemTime::now();

        // The stream is already established when this adapter is created.
        let state =
            crate::adapters::core::ConnState::new(crate::adapters::core::ConnStatus::Connected);

        let (send_queue_tx, send_queue_rx) = mpsc::channel(limits.outbound_capacity);
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();
        // Bounded event backbone: the loop task owns the sender half; when the
        // loop ends the pipe drops and the consumer sees end-of-data.
        let (event_pipe, event_pipe_rx) = crate::adapters::events::event_pipe(limits.pipe_capacity);

        let memory_pool = shared_memory_pool();
        let frame_policy = Arc::new(std::sync::atomic::AtomicU8::new(
            crate::packet::FramePolicy::default() as u8,
        ));
        // Idle timeout comes from the server config (client config has no such
        // field); previously the option existed but nothing enforced it.
        let idle_timeout = (&config as &dyn std::any::Any)
            .downcast_ref::<TcpServerConfig>()
            .and_then(|c| c.idle_timeout);

        let event_loop_handle = Self::start_event_loop(
            stream,
            state.clone(),
            send_queue_rx,
            shutdown_rx,
            event_pipe,
            memory_pool,
            frame_policy.clone(),
            idle_timeout,
            limits.write_deadline,
        )
        .await;

        Ok(Self {
            state,
            config,
            connection_info,
            send_queue: send_queue_tx,
            event_pipe_rx: Some(event_pipe_rx),
            shutdown_sender: shutdown_tx,
            event_loop_handle: Some(event_loop_handle),
            frame_policy,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_event_loop(
        stream: TcpStream,
        state: crate::adapters::core::ConnState,
        mut send_queue: mpsc::Receiver<crate::adapters::outbound::Outbound>,
        mut shutdown_signal: mpsc::UnboundedReceiver<()>,
        event_pipe: crate::adapters::events::EventPipe,
        memory_pool: Arc<OptimizedMemoryPool>,
        frame_policy: Arc<std::sync::atomic::AtomicU8>,
        idle_timeout: Option<std::time::Duration>,
        write_deadline: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let current_session_id = state.session_id();
            tracing::debug!(
                "[START] TCP event loop started (session: {})",
                current_session_id
            );

            let (mut read_half, mut write_half) = stream.into_split();
            let mut read_buffer = OptimizedReadBuffer::new_with_pool(8192, memory_pool);
            // Any traffic in either direction counts as activity.
            let mut last_activity = tokio::time::Instant::now();

            'event_loop: loop {
                // Get current session ID
                let current_session_id = state.session_id();

                tokio::select! {
                    // [IDLE] Enforce the configured idle timeout: a connection
                    // with no traffic in either direction for the duration is
                    // closed (previously the option was accepted and ignored).
                    _ = tokio::time::sleep_until(last_activity + idle_timeout.unwrap_or_default()), if idle_timeout.is_some() => {
                        tracing::info!("[IDLE] TCP connection idle for {:?}, closing (session: {})", idle_timeout.unwrap(), current_session_id);
                        event_pipe.close(crate::error::CloseReason::Timeout);
                        break;
                    }
                    // [RECV] Handle receive data - using optimized buffer method
                    read_result = read_buffer.fill_from_stream(&mut read_half) => {
                        match read_result {
                            Ok(0) => {
                                tracing::debug!("[RECV] Peer actively closed TCP connection (session: {})", current_session_id);
                                // Peer actively closed: notify upper layer that connection is closed for resource cleanup
                                event_pipe.close(crate::error::CloseReason::Normal);
                                break;
                            }
                            Ok(_) => {
                                last_activity = tokio::time::Instant::now();
                                // Parse all complete packets currently in the buffer.
                                let strict = crate::packet::FramePolicy::from_u8(
                                    frame_policy.load(std::sync::atomic::Ordering::Relaxed),
                                ) == crate::packet::FramePolicy::Strict;
                                loop {
                                    match read_buffer.try_parse_next_packet(strict) {
                                        Ok(Some(packet)) => {
                                            tracing::debug!("[RECV] TCP received packet: {} bytes (session: {})", packet.payload.len(), current_session_id);
                                            tracing::debug!("[DETAIL] Packet details: ID={}, type={:?}, payload_len={}", packet.header.message_id, packet.header.packet_type, packet.payload.len());

                                            // Data plane: backpressure. A slow
                                            // consumer parks the read loop here,
                                            // which parks the socket via TCP
                                            // flow control — no event is dropped.
                                            if !event_pipe
                                                .deliver(TransportEvent::MessageReceived(packet))
                                                .await
                                            {
                                                tracing::debug!("[RECV] Event consumer gone (session: {})", current_session_id);
                                                break 'event_loop;
                                            }
                                        }
                                        Ok(None) => break,
                                        Err(e) => {
                                            tracing::error!("[RECV] TCP parse error: {:?} (session: {})", e, current_session_id);
                                            event_pipe.close(crate::error::CloseReason::Error(format!("{:?}", e)));
                                            break 'event_loop;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("[RECV] TCP connection error: {:?} (session: {})", e, current_session_id);
                                // Network error: notify upper layer of connection error for resource cleanup
                                event_pipe.close(crate::error::CloseReason::Error(format!("{:?}", e)));
                                break;
                            }
                        }
                    }

                    // [SEND] Handle send data
                    item = send_queue.recv() => {
                        if let Some(item) = item {
                            let (packet, completion) = (item.packet, item.completion);
                            // A write that exceeds the deadline means the peer stopped
                            // draining: the connection is declared dead rather than
                            // blocking every queued sender behind it.
                            let write = tokio::time::timeout(
                                write_deadline,
                                Self::write_packet_to_stream(&mut write_half, &packet),
                            ).await;
                            match write {
                                Ok(Ok(_)) => {
                                    last_activity = tokio::time::Instant::now();
                                    tracing::debug!("[SEND] TCP send successful: {} bytes (session: {})", packet.payload.len(), current_session_id);
                                    // Write completion: the REAL result, after the write.
                                    if let Some(completion) = completion {
                                        completion.complete(Ok(()));
                                    }
                                    // Diagnostic tier: droppable under load.
                                    event_pipe.diagnostic(TransportEvent::MessageSent { packet_id: packet.header.message_id });
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("[SEND] TCP send error: {:?} (session: {})", e, current_session_id);
                                    if let Some(completion) = completion {
                                        completion.complete(Err(crate::error::TransportError::connection_error(
                                            "TCP write failed",
                                            false,
                                        )));
                                    }
                                    // Send error: notify upper layer of connection error for resource cleanup
                                    event_pipe.close(crate::error::CloseReason::Error(format!("{:?}", e)));
                                    break;
                                }
                                Err(_) => {
                                    tracing::error!("[SEND] TCP write deadline exceeded (session: {})", current_session_id);
                                    if let Some(completion) = completion {
                                        completion.complete(Err(crate::error::TransportError::connection_error(
                                            "TCP write deadline exceeded",
                                            false,
                                        )));
                                    }
                                    event_pipe.close(crate::error::CloseReason::Error(
                                        "TCP write deadline exceeded".to_string(),
                                    ));
                                    break;
                                }
                            }
                        }
                    }

                    // [STOP] Handle shutdown signal
                    _ = shutdown_signal.recv() => {
                        tracing::info!("[STOP] Received shutdown signal, stopping TCP event loop (session: {})", current_session_id);
                        // Locally initiated close: publish the reason so the
                        // consumer's single ConnectionClosed carries Normal
                        // instead of the synthesized abnormal-end reason.
                        event_pipe.close(crate::error::CloseReason::Normal);
                        break;
                    }
                }
            }

            // The loop has ended (peer close, error, or shutdown): mark closed so
            // is_connected() reflects reality, not just what close() sets.
            state.set_status(crate::adapters::core::ConnStatus::Closed);

            tracing::debug!(
                "[SUCCESS] TCP event loop ended (session: {})",
                current_session_id
            );
        })
    }

    /// Write packet to stream (zero-copy optimized)
    async fn write_packet_to_stream(
        write_half: &mut tokio::net::tcp::OwnedWriteHalf,
        packet: &Packet,
    ) -> Result<(), TcpError> {
        // Fallible encode: an unencodable packet (ext header > u16::MAX or
        // payload > u32::MAX) is a protocol error, not a panic.
        let packet_bytes = packet
            .try_encode()
            .map_err(|e| TcpError::Config(format!("encode failed: {e}")))?;
        write_half
            .write_all(&packet_bytes)
            .await
            .map_err(TcpError::Io)?;
        // Note: Flush removed for batching - let TCP Nagle or explicit flush handle it
        Ok(())
    }
}

// Client adapter implementation
impl TcpAdapter<TcpClientConfig> {
    /// Connect to TCP server
    pub async fn connect(
        addr: std::net::SocketAddr,
        config: TcpClientConfig,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self, TcpError> {
        tracing::debug!("[CONNECT] TCP client connecting to: {}", addr);

        // Optional local bind (previously silently ignored): connect through a
        // TcpSocket so the source address/port can be pinned.
        let connect = async {
            match config.local_bind_address {
                Some(local) => {
                    let socket = if addr.is_ipv4() {
                        tokio::net::TcpSocket::new_v4()?
                    } else {
                        tokio::net::TcpSocket::new_v6()?
                    };
                    socket.bind(local)?;
                    socket.connect(addr).await
                }
                None => TcpStream::connect(addr).await,
            }
        };
        let stream = if config.connect_timeout != std::time::Duration::from_secs(0) {
            tokio::time::timeout(config.connect_timeout, connect)
                .await
                .map_err(|_| TcpError::Timeout)?
                .map_err(TcpError::Io)?
        } else {
            connect.await.map_err(TcpError::Io)?
        };

        tracing::debug!("[SUCCESS] TCP connection established successfully");

        if let Some(keepalive) = config.keepalive {
            apply_tcp_keepalive(&stream, keepalive);
        }

        let nodelay = config.nodelay;
        Self::new(stream, config, nodelay, limits).await
    }
}

#[async_trait]
impl<C: Send + Sync + 'static> Connection for TcpAdapter<C> {
    fn writer(&self) -> std::sync::Arc<dyn crate::connection::ConnectionWriter> {
        std::sync::Arc::new(crate::adapters::outbound::QueueWriter::new(
            self.send_queue.clone(),
            "tcp_outbound_queue",
            "TCP connection closed",
        ))
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.shutdown_sender.send(());
        if let Some(handle) = self.event_loop_handle.take() {
            let _ = handle.await;
        }
        self.state
            .set_status(crate::adapters::core::ConnStatus::Closed);
        self.connection_info.state = ConnectionState::Closed;
        self.connection_info.closed_at = Some(std::time::SystemTime::now());
        Ok(())
    }

    fn session_id(&self) -> SessionId {
        self.state.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        self.state.set_session_id(session_id);
        self.connection_info.session_id = session_id;
    }

    fn connection_info(&self) -> ConnectionInfo {
        self.connection_info.clone()
    }

    fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    fn take_event_pipe(&mut self) -> Option<crate::spi::ConnectionEvents> {
        self.event_pipe_rx.take().map(crate::spi::ConnectionEvents)
    }

    fn set_frame_policy(&self, policy: crate::packet::FramePolicy) {
        self.frame_policy
            .store(policy as u8, std::sync::atomic::Ordering::Relaxed);
    }
}

/// TCP server builder
pub(crate) struct TcpServerBuilder {
    config: TcpServerConfig,
    bind_address: Option<std::net::SocketAddr>,
    limits: crate::transport::limits::ConnectionLimits,
}

impl TcpServerBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: TcpServerConfig::default(),
            bind_address: None,
            limits: crate::transport::limits::ConnectionLimits::default(),
        }
    }

    pub(crate) fn limits(mut self, limits: crate::transport::limits::ConnectionLimits) -> Self {
        self.limits = limits;
        self
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

        // Bind through a TcpSocket so reuse_addr is actually honored
        // (previously the config flag was silently ignored and tokio's
        // default was whatever the platform picked).
        let socket = if bind_addr.is_ipv4() {
            tokio::net::TcpSocket::new_v4()?
        } else {
            tokio::net::TcpSocket::new_v6()?
        };
        socket.set_reuseaddr(self.config.reuse_addr)?;
        socket.bind(bind_addr)?;
        let listener = socket.listen(1024)?;

        tracing::info!(
            "[SUCCESS] TCP server successfully started on: {}",
            listener.local_addr()?
        );

        Ok(TcpServer {
            listener: Some(listener),
            config: self.config,
            limits: self.limits,
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
    listener: Option<TcpListener>,
    config: TcpServerConfig,
    limits: crate::transport::limits::ConnectionLimits,
}

impl TcpServer {
    pub(crate) async fn accept(&mut self) -> Result<TcpAdapter<TcpServerConfig>, TcpError> {
        let listener = self
            .listener
            .as_mut()
            .ok_or_else(|| TcpError::Config("TCP server is shut down".to_string()))?;
        let (stream, peer_addr) = listener.accept().await?;

        tracing::debug!("[CONNECT] TCP new connection from: {}", peer_addr);

        if let Some(keepalive) = self.config.keepalive {
            apply_tcp_keepalive(&stream, keepalive);
        }

        TcpAdapter::new(
            stream,
            self.config.clone(),
            self.config.nodelay,
            self.limits,
        )
        .await
    }

    pub(crate) fn local_addr(&self) -> Result<std::net::SocketAddr, TcpError> {
        let listener = self
            .listener
            .as_ref()
            .ok_or_else(|| TcpError::Config("TCP server is shut down".to_string()))?;
        Ok(listener.local_addr()?)
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), TcpError> {
        // Explicitly drop listener to release port without waiting for task drop.
        self.listener.take();
        Ok(())
    }
}

/// TCP client builder
pub(crate) struct TcpClientBuilder {
    config: TcpClientConfig,
    target_address: Option<std::net::SocketAddr>,
    limits: crate::transport::limits::ConnectionLimits,
}

impl TcpClientBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: TcpClientConfig::default(),
            target_address: None,
            limits: crate::transport::limits::ConnectionLimits::default(),
        }
    }

    pub(crate) fn limits(mut self, limits: crate::transport::limits::ConnectionLimits) -> Self {
        self.limits = limits;
        self
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
        TcpAdapter::connect(target_addr, self.config, self.limits).await
    }
}

impl Default for TcpClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod strict_stream_tests {
    use super::*;

    fn framer() -> OptimizedReadBuffer {
        OptimizedReadBuffer::new_with_pool(8192, shared_memory_pool())
    }

    fn feed(f: &mut OptimizedReadBuffer, bytes: &[u8]) {
        f.buffer.extend_from_slice(bytes);
    }

    /// Strict: an invalid header closes the connection even AFTER valid
    /// packets were parsed — a desynchronized byte stream is not trustworthy.
    /// (1.x only fast-failed on the FIRST packet and resynced afterwards.)
    #[test]
    fn strict_fails_on_corruption_after_valid_packets() {
        let mut f = framer();
        feed(
            &mut f,
            &Packet::one_way(1, b"ok".to_vec()).try_encode().unwrap(),
        );
        let first = f.try_parse_next_packet(true).expect("valid stream");
        assert_eq!(first.expect("complete").message_id(), 1);

        feed(&mut f, &[0xFFu8; 32]); // garbage mid-stream
        assert!(
            f.try_parse_next_packet(true).is_err(),
            "strict must close on any invalid header, not resync"
        );
    }

    /// Lenient keeps the 1.x behavior: bounded resync recovers the next
    /// valid frame after mid-stream corruption.
    #[test]
    fn lenient_resyncs_after_corruption() {
        let mut f = framer();
        feed(
            &mut f,
            &Packet::one_way(1, b"ok".to_vec()).try_encode().unwrap(),
        );
        assert!(f.try_parse_next_packet(false).expect("ok").is_some());

        feed(&mut f, &[0xFFu8; 8]); // corruption
        feed(
            &mut f,
            &Packet::one_way(2, b"back".to_vec()).try_encode().unwrap(),
        );
        let recovered = f
            .try_parse_next_packet(false)
            .expect("lenient stream survives")
            .expect("resynced packet");
        assert_eq!(recovered.message_id(), 2);
    }

    /// Both policies fast-fail non-protocol traffic on the very first bytes.
    #[test]
    fn first_invalid_header_fails_under_both_policies() {
        for strict in [true, false] {
            let mut f = framer();
            feed(&mut f, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
            assert!(f.try_parse_next_packet(strict).is_err());
        }
    }

    /// An oversized declared frame is refused up front (FrameTooLarge from
    /// the shared codec), instead of waiting for ~1 MiB+ that must never be
    /// accepted.
    #[test]
    fn oversized_declared_frame_fails_fast_under_strict() {
        let mut f = framer();
        let mut bytes = Packet::one_way(1, b"x".to_vec())
            .try_encode()
            .unwrap()
            .to_vec();
        bytes[10] = 0xFF;
        bytes[11] = 0xFF;
        bytes[12] = 0xFF;
        bytes[13] = 0xFF;
        feed(&mut f, &bytes);
        assert!(f.try_parse_next_packet(true).is_err());
    }

    /// The TCP-specific payload cap (1 MiB) bites from the header ALONE:
    /// payload_len = 1 MiB + 1 with only the 16-byte header buffered is
    /// rejected immediately under BOTH policies — no waiting for the payload,
    /// no resync into it, even after valid packets were parsed.
    #[test]
    fn payload_one_over_tcp_cap_rejects_from_header_alone_under_both_policies() {
        for strict in [true, false] {
            let mut f = framer();
            // Establish a valid stream first so this is not the first-packet
            // fast-fail path.
            feed(
                &mut f,
                &Packet::one_way(1, b"ok".to_vec()).try_encode().unwrap(),
            );
            assert!(f.try_parse_next_packet(strict).expect("ok").is_some());

            let mut header = Packet::one_way(2, Vec::new())
                .try_encode()
                .unwrap()
                .to_vec();
            header[10..14].copy_from_slice(&((MAX_PAYLOAD_SIZE as u32) + 1).to_be_bytes());
            assert_eq!(header.len(), FIXED_HEADER_SIZE, "header only, no payload");
            feed(&mut f, &header);
            assert!(
                f.try_parse_next_packet(strict).is_err(),
                "strict={strict}: oversized payload must be refused from the header"
            );
        }
    }
}

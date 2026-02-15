use crate::{
    command::ConnectionInfo, error::TransportError, event::TransportEvent, packet::Packet,
    Connection, SessionId,
};
/// Lock-free connection object - Solves Arc<Mutex<Connection>> lock contention issues
///
/// Design approach:
/// 1. Use Channel queues to replace direct method calls, avoiding lock contention
/// 2. Connection state managed with atomic variables
/// 3. Send operations implemented through lock-free queues
/// 4. Statistics use atomic counters
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, oneshot};

/// Lock-free connection commands
#[derive(Debug)]
pub enum LockFreeConnectionCommand {
    /// Send packet
    Send {
        packet: Packet,
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },
    /// Close connection
    Close {
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },
    /// Flush buffer
    Flush {
        response_tx: oneshot::Sender<Result<(), TransportError>>,
    },
    /// Check connection status
    IsConnected { response_tx: oneshot::Sender<bool> },
    /// Get connection information
    GetConnectionInfo {
        response_tx: oneshot::Sender<ConnectionInfo>,
    },
}

/// Lock-free connection statistics
#[derive(Debug)]
pub struct LockFreeConnectionStats {
    /// Number of packets sent
    pub packets_sent: AtomicU64,
    /// Number of bytes sent
    pub bytes_sent: AtomicU64,
    /// Number of send failures
    pub send_failures: AtomicU64,
    /// Command queue depth
    pub queue_depth: AtomicU64,
    /// Last activity time
    pub last_activity: AtomicU64,
    /// Connection created time
    pub created_at: AtomicU64,
}

impl LockFreeConnectionStats {
    pub fn new() -> Self {
        let now = Instant::now().elapsed().as_nanos() as u64;
        Self {
            packets_sent: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            send_failures: AtomicU64::new(0),
            queue_depth: AtomicU64::new(0),
            last_activity: AtomicU64::new(now),
            created_at: AtomicU64::new(now),
        }
    }

    pub fn record_packet_sent(&self, bytes: usize) {
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
        self.last_activity.store(
            Instant::now().elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );
    }

    pub fn record_send_failure(&self) {
        self.send_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn update_queue_depth(&self, depth: u64) {
        self.queue_depth.store(depth, Ordering::Relaxed);
    }
}

/// Lock-free connection object - Core implementation
///
/// This structure is completely lock-free, implementing all operations through message passing
pub struct LockFreeConnection {
    /// Session ID (atomic operation)
    session_id: AtomicU64,
    /// Connection state (atomic operation)
    is_connected: AtomicBool,
    /// Command sender (lock-free)
    command_tx: flume::Sender<LockFreeConnectionCommand>,
    /// Event broadcaster
    event_tx: broadcast::Sender<TransportEvent>,
    /// Statistics information (atomic operations)
    stats: Arc<LockFreeConnectionStats>,
    /// Cached connection information
    cached_info: ConnectionInfo,
}

impl LockFreeConnection {
    /// Create new lock-free connection
    ///
    /// Parameters:
    /// - connection: Underlying connection object
    /// - session_id: Session ID
    /// - buffer_size: Command queue buffer size
    pub fn new(
        mut connection: Box<dyn Connection>,
        session_id: SessionId,
        buffer_size: usize,
    ) -> (Self, tokio::task::JoinHandle<()>) {
        let (command_tx, command_rx) = flume::bounded(buffer_size.max(1));
        let (event_tx, _) = broadcast::channel(buffer_size);
        let stats = Arc::new(LockFreeConnectionStats::new());

        let cached_info = connection.connection_info();

        // [BRIDGE] Bridge underlying connection event stream to lock-free connection event channel
        let event_tx_for_bridge = event_tx.clone();
        if let Some(mut connection_events) = connection.event_stream() {
            tokio::spawn(async move {
                tracing::debug!("[BRIDGE] Starting event bridge (session: {})", session_id);
                while let Ok(event) = connection_events.recv().await {
                    tracing::trace!("[BRIDGE] Bridging event: {:?}", event);
                    if let Err(_) = event_tx_for_bridge.send(event) {
                        tracing::debug!("[BRIDGE] Event bridge stopped - receiver disconnected");
                        break;
                    }
                }
                tracing::debug!("[BRIDGE] Event bridge ended (session: {})", session_id);
            });
        } else {
            tracing::debug!(
                "[BRIDGE] No event stream from underlying connection, skipping event bridge"
            );
        }

        let lockfree_conn = Self {
            session_id: AtomicU64::new(session_id.0),
            is_connected: AtomicBool::new(connection.is_connected()),
            command_tx,
            event_tx: event_tx.clone(),
            stats: stats.clone(),
            cached_info,
        };

        // Start background processing task
        let handle = tokio::spawn(Self::connection_worker(
            connection, command_rx, event_tx, stats,
        ));

        (lockfree_conn, handle)
    }

    /// Background connection worker - Handles all actual connection operations
    async fn connection_worker(
        mut connection: Box<dyn Connection>,
        command_rx: flume::Receiver<LockFreeConnectionCommand>,
        event_tx: broadcast::Sender<TransportEvent>,
        stats: Arc<LockFreeConnectionStats>,
    ) {
        tracing::debug!(
            "[START] Starting lock-free connection worker (session: {})",
            connection.session_id()
        );

        // Process command queue
        while let Ok(command) = command_rx.recv_async().await {
            stats.update_queue_depth(command_rx.len() as u64);

            match command {
                LockFreeConnectionCommand::Send {
                    packet,
                    response_tx,
                } => {
                    let packet_size = packet.payload.len();

                    // Add timeout for send operation to prevent worker from getting stuck
                    let send_result = tokio::time::timeout(
                        std::time::Duration::from_secs(5), // 5 second timeout
                        connection.send(packet),
                    )
                    .await;

                    match send_result {
                        Ok(Ok(_)) => {
                            stats.record_packet_sent(packet_size);
                            let _ = response_tx.send(Ok(()));
                        }
                        Ok(Err(e)) => {
                            stats.record_send_failure();
                            let _ = response_tx.send(Err(e));
                        }
                        Err(_) => {
                            // Timeout error
                            stats.record_send_failure();
                            let _ = response_tx.send(Err(TransportError::connection_error(
                                "Send operation timeout",
                                false,
                            )));
                        }
                    }
                }

                LockFreeConnectionCommand::Close { response_tx } => {
                    // Also add timeout for close operation
                    let close_result = tokio::time::timeout(
                        std::time::Duration::from_secs(3), // 3 second timeout
                        connection.close(),
                    )
                    .await;

                    match close_result {
                        Ok(result) => {
                            let should_break = result.is_ok();
                            let _ = response_tx.send(result);

                            // Exit worker after close
                            if should_break {
                                let _ = event_tx.send(TransportEvent::ConnectionClosed {
                                    reason: crate::CloseReason::Normal,
                                });
                                break;
                            }
                        }
                        Err(_) => {
                            // Timeout, force close
                            let _ = response_tx.send(Err(TransportError::connection_error(
                                "Close operation timeout",
                                false,
                            )));
                            break; // Also exit worker on timeout
                        }
                    }
                }

                LockFreeConnectionCommand::Flush { response_tx } => {
                    // Add timeout for flush operation
                    let flush_result = tokio::time::timeout(
                        std::time::Duration::from_secs(2), // 2 second timeout
                        connection.flush(),
                    )
                    .await;

                    match flush_result {
                        Ok(result) => {
                            let _ = response_tx.send(result);
                        }
                        Err(_) => {
                            let _ = response_tx.send(Err(TransportError::connection_error(
                                "Flush operation timeout",
                                false,
                            )));
                        }
                    }
                }

                LockFreeConnectionCommand::IsConnected { response_tx } => {
                    let is_connected = connection.is_connected();
                    let _ = response_tx.send(is_connected);
                }

                LockFreeConnectionCommand::GetConnectionInfo { response_tx } => {
                    let info = connection.connection_info();
                    let _ = response_tx.send(info);
                }
            }
        }

        tracing::debug!(
            "[END] Lock-free connection worker ended (session: {})",
            connection.session_id()
        );
    }

    /// [PERF] Lock-free send - Core optimization method
    ///
    /// Implemented through message queues, completely avoiding lock contention
    pub async fn send_lockfree(&self, packet: Packet) -> Result<(), TransportError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = LockFreeConnectionCommand::Send {
            packet,
            response_tx,
        };

        // Non-blocking send command
        self.command_tx
            .send_async(command)
            .await
            .map_err(|_| TransportError::connection_error("Connection closed", false))?;

        // Wait for response
        response_rx
            .await
            .map_err(|_| TransportError::connection_error("Response channel closed", false))?
    }

    /// [PERF] Lock-free close
    pub async fn close_lockfree(&self) -> Result<(), TransportError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = LockFreeConnectionCommand::Close { response_tx };

        self.command_tx
            .send_async(command)
            .await
            .map_err(|_| TransportError::connection_error("Connection closed", false))?;

        let result = response_rx
            .await
            .map_err(|_| TransportError::connection_error("Response channel closed", false))?;

        if result.is_ok() {
            self.is_connected.store(false, Ordering::SeqCst);
        }

        result
    }

    /// [PERF] Lock-free flush
    pub async fn flush_lockfree(&self) -> Result<(), TransportError> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = LockFreeConnectionCommand::Flush { response_tx };

        self.command_tx
            .send_async(command)
            .await
            .map_err(|_| TransportError::connection_error("Connection closed", false))?;

        response_rx
            .await
            .map_err(|_| TransportError::connection_error("Response channel closed", false))?
    }

    /// [PERF] Lock-free status check - Uses atomic operations, ultra-fast
    pub fn is_connected_lockfree(&self) -> bool {
        self.is_connected.load(Ordering::Relaxed)
    }

    /// Get session ID - Atomic operation
    pub fn session_id_lockfree(&self) -> SessionId {
        SessionId(self.session_id.load(Ordering::Relaxed))
    }

    /// Set session ID - Atomic operation
    pub fn set_session_id_lockfree(&self, session_id: SessionId) {
        self.session_id.store(session_id.0, Ordering::SeqCst);
    }

    /// Get connection information - Uses cache to avoid async calls
    pub fn connection_info_lockfree(&self) -> ConnectionInfo {
        self.cached_info.clone()
    }

    /// Subscribe to event stream  
    pub fn subscribe_events(&self) -> broadcast::Receiver<TransportEvent> {
        self.event_tx.subscribe()
    }

    /// Get statistics information
    pub fn stats(&self) -> &LockFreeConnectionStats {
        &self.stats
    }

    /// Get command queue depth
    pub fn queue_depth(&self) -> usize {
        self.stats.queue_depth.load(Ordering::Relaxed) as usize
    }

    /// Check if connection is healthy
    pub fn is_healthy(&self) -> bool {
        self.is_connected_lockfree() && self.queue_depth() < 1000 // Queue cannot be too deep
    }
}

/// Implement Clone for convenient use of the same connection in multiple places
impl Clone for LockFreeConnection {
    fn clone(&self) -> Self {
        Self {
            session_id: AtomicU64::new(self.session_id.load(Ordering::Relaxed)),
            is_connected: AtomicBool::new(self.is_connected.load(Ordering::Relaxed)),
            command_tx: self.command_tx.clone(),
            event_tx: self.event_tx.clone(),
            stats: self.stats.clone(),
            cached_info: self.cached_info.clone(),
        }
    }
}

/// Implement Send + Sync for compatibility
unsafe impl Send for LockFreeConnection {}
unsafe impl Sync for LockFreeConnection {}

/// Implement Connection trait for compatibility with existing systems
#[async_trait::async_trait]
impl crate::Connection for LockFreeConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        self.send_lockfree(packet).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.close_lockfree().await
    }

    fn session_id(&self) -> SessionId {
        self.session_id_lockfree()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        self.set_session_id_lockfree(session_id)
    }

    fn connection_info(&self) -> ConnectionInfo {
        self.connection_info_lockfree()
    }

    fn is_connected(&self) -> bool {
        self.is_connected_lockfree()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        self.flush_lockfree().await
    }

    fn event_stream(&self) -> Option<broadcast::Receiver<TransportEvent>> {
        Some(self.subscribe_events())
    }
}

/// [TARGET] Batch operation support - Further performance optimization
impl LockFreeConnection {
    /// Send multiple packets in batch
    pub async fn send_batch_lockfree(
        &self,
        packets: Vec<Packet>,
    ) -> Vec<Result<(), TransportError>> {
        let mut results = Vec::with_capacity(packets.len());

        // Send all packets concurrently
        let mut tasks = Vec::new();

        for packet in packets {
            let conn = self.clone();
            let task = tokio::spawn(async move { conn.send_lockfree(packet).await });
            tasks.push(task);
        }

        // Wait for all tasks to complete
        for task in tasks {
            match task.await {
                Ok(result) => results.push(result),
                Err(_) => results.push(Err(TransportError::connection_error(
                    "Task execution failed",
                    false,
                ))),
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::PacketType;
    use bytes::Bytes;

    // Mock connection for testing
    struct MockConnection {
        session_id: SessionId,
        is_connected: bool,
    }

    #[async_trait::async_trait]
    impl Connection for MockConnection {
        async fn send(&mut self, _packet: Packet) -> Result<(), TransportError> {
            Ok(())
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            self.is_connected = false;
            Ok(())
        }

        fn session_id(&self) -> SessionId {
            self.session_id
        }

        fn set_session_id(&mut self, session_id: SessionId) {
            self.session_id = session_id;
        }

        fn connection_info(&self) -> ConnectionInfo {
            ConnectionInfo::default()
        }

        fn is_connected(&self) -> bool {
            self.is_connected
        }

        async fn flush(&mut self) -> Result<(), TransportError> {
            Ok(())
        }

        fn event_stream(&self) -> Option<broadcast::Receiver<TransportEvent>> {
            None
        }
    }

    #[tokio::test]
    async fn test_lockfree_connection_send() {
        let mock_conn = Box::new(MockConnection {
            session_id: SessionId(1),
            is_connected: true,
        });

        let (lockfree_conn, _handle) = LockFreeConnection::new(mock_conn, SessionId(1), 100);

        let packet = Packet {
            header: crate::packet::FixedHeader {
                version: 1,
                compression: crate::packet::CompressionType::None,
                packet_type: PacketType::Data,
                biz_type: 0,
                message_id: 1,
                ext_header_len: 0,
                payload_len: 4,
                reserved: crate::packet::ReservedFlags::new(),
            },
            payload: b"test".to_vec(),
            ext_header: vec![],
        };

        let result = lockfree_conn.send_lockfree(packet).await;
        assert!(result.is_ok());

        // Check statistics
        assert_eq!(
            lockfree_conn.stats().packets_sent.load(Ordering::Relaxed),
            1
        );
        assert_eq!(lockfree_conn.stats().bytes_sent.load(Ordering::Relaxed), 4);
    }

    #[tokio::test]
    async fn test_lockfree_connection_concurrent_sends() {
        let mock_conn = Box::new(MockConnection {
            session_id: SessionId(1),
            is_connected: true,
        });

        let (lockfree_conn, _handle) = LockFreeConnection::new(mock_conn, SessionId(1), 100);

        // Send 100 packets concurrently
        let mut tasks = Vec::new();

        for i in 0..100 {
            let conn = lockfree_conn.clone();
            let task = tokio::spawn(async move {
                let packet = Packet {
                    header: crate::packet::FixedHeader {
                        version: 1,
                        compression: crate::packet::CompressionType::None,
                        packet_type: PacketType::Data,
                        biz_type: 0,
                        message_id: i,
                        ext_header_len: 0,
                        payload_len: 4,
                        reserved: crate::packet::ReservedFlags::new(),
                    },
                    payload: b"test".to_vec(),
                    ext_header: vec![],
                };

                conn.send_lockfree(packet).await
            });
            tasks.push(task);
        }

        // Wait for all tasks to complete
        for task in tasks {
            let result = task.await.unwrap();
            assert!(result.is_ok());
        }

        // Check statistics
        assert_eq!(
            lockfree_conn.stats().packets_sent.load(Ordering::Relaxed),
            100
        );
    }
}

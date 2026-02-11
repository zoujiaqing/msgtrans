use crate::{command::ConnectionInfo, error::TransportError, packet::Packet, SessionId};
use flume::{Receiver as FlumeReceiver, Sender as FlumeSender};
/// [PROTOCOL] Flume-powered asynchronous protocol adapter
///
/// Core optimizations:
/// 1. Flume async channels replace traditional async/await patterns
/// 2. Batch processing improves throughput
/// 3. LockFree statistics monitoring
/// 4. Zero-copy data transmission
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// LockFree protocol statistics
#[derive(Debug)]
pub struct LockFreeProtocolStats {
    // Send statistics
    pub packets_sent: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub send_errors: AtomicU64,

    // Receive statistics
    pub packets_received: AtomicU64,
    pub bytes_received: AtomicU64,
    pub receive_errors: AtomicU64,

    // Batch processing statistics
    pub batch_operations: AtomicU64,
    pub total_batch_size: AtomicU64,
    pub max_batch_size: AtomicUsize,

    // Performance statistics
    pub total_latency_ns: AtomicU64,
    pub operation_count: AtomicU64,

    // Cache statistics
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
}

impl LockFreeProtocolStats {
    pub fn new() -> Self {
        Self {
            packets_sent: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            send_errors: AtomicU64::new(0),
            packets_received: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            receive_errors: AtomicU64::new(0),
            batch_operations: AtomicU64::new(0),
            total_batch_size: AtomicU64::new(0),
            max_batch_size: AtomicUsize::new(0),
            total_latency_ns: AtomicU64::new(0),
            operation_count: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
        }
    }

    pub fn record_send(&self, bytes: usize) {
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn record_receive(&self, bytes: usize) {
        self.packets_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn record_batch(&self, batch_size: usize) {
        self.batch_operations.fetch_add(1, Ordering::Relaxed);
        self.total_batch_size
            .fetch_add(batch_size as u64, Ordering::Relaxed);

        // Atomically update maximum batch size
        self.max_batch_size.fetch_max(batch_size, Ordering::Relaxed);
    }

    pub fn record_operation_latency(&self, latency_ns: u64) {
        self.total_latency_ns
            .fetch_add(latency_ns, Ordering::Relaxed);
        self.operation_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get statistics snapshot
    pub fn snapshot(&self) -> ProtocolStatsSnapshot {
        ProtocolStatsSnapshot {
            packets_sent: self.packets_sent.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            packets_received: self.packets_received.load(Ordering::Relaxed),
            bytes_received: self.bytes_received.load(Ordering::Relaxed),
            receive_errors: self.receive_errors.load(Ordering::Relaxed),
            batch_operations: self.batch_operations.load(Ordering::Relaxed),
            total_batch_size: self.total_batch_size.load(Ordering::Relaxed),
            max_batch_size: self.max_batch_size.load(Ordering::Relaxed),
            total_latency_ns: self.total_latency_ns.load(Ordering::Relaxed),
            operation_count: self.operation_count.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
        }
    }
}

/// Statistics snapshot
#[derive(Debug, Clone)]
pub struct ProtocolStatsSnapshot {
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub send_errors: u64,
    pub packets_received: u64,
    pub bytes_received: u64,
    pub receive_errors: u64,
    pub batch_operations: u64,
    pub total_batch_size: u64,
    pub max_batch_size: usize,
    pub total_latency_ns: u64,
    pub operation_count: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

impl ProtocolStatsSnapshot {
    pub fn average_latency_ns(&self) -> f64 {
        if self.operation_count > 0 {
            self.total_latency_ns as f64 / self.operation_count as f64
        } else {
            0.0
        }
    }

    pub fn average_batch_size(&self) -> f64 {
        if self.batch_operations > 0 {
            self.total_batch_size as f64 / self.batch_operations as f64
        } else {
            0.0
        }
    }

    pub fn cache_hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total > 0 {
            self.cache_hits as f64 / total as f64
        } else {
            0.0
        }
    }

    pub fn throughput_pps(&self, duration: Duration) -> f64 {
        let total_packets = self.packets_sent + self.packets_received;
        total_packets as f64 / duration.as_secs_f64()
    }

    pub fn bandwidth_bps(&self, duration: Duration) -> f64 {
        let total_bytes = self.bytes_sent + self.bytes_received;
        total_bytes as f64 / duration.as_secs_f64()
    }
}

/// Protocol event types
#[derive(Debug, Clone)]
pub enum ProtocolEvent {
    PacketSent(usize),         // Packet sent, parameter is byte count
    PacketReceived(usize),     // Packet received, parameter is byte count
    BatchProcessed(usize),     // Batch processed, parameter is batch size
    PerformanceMetric(String), // Performance metric
    Error(String),             // Error event
    ConnectionStatus(bool),    // Connection status
}

/// [OPTIMIZED] Flume-driven protocol adapter
pub struct FlumePoweredProtocolAdapter {
    // Flume send channel
    send_tx: FlumeSender<Packet>,
    send_rx: FlumeReceiver<Packet>,

    // Flume receive channel
    recv_tx: FlumeSender<Packet>,
    recv_rx: FlumeReceiver<Packet>,

    // Event broadcast
    event_tx: FlumeSender<ProtocolEvent>,

    // LockFree statistics
    stats: Arc<LockFreeProtocolStats>,

    // Session information
    session_id: SessionId,
    connection_info: ConnectionInfo,
    is_connected: std::sync::atomic::AtomicBool,
}

impl FlumePoweredProtocolAdapter {
    /// Create new Flume protocol adapter
    pub fn new(
        session_id: SessionId,
        connection_info: ConnectionInfo,
    ) -> (Self, FlumeReceiver<ProtocolEvent>) {
        let (send_tx, send_rx) = flume::unbounded();
        let (recv_tx, recv_rx) = flume::unbounded();
        let (event_tx, event_rx) = flume::unbounded();

        let adapter = Self {
            send_tx,
            send_rx,
            recv_tx,
            recv_rx,
            event_tx,
            stats: Arc::new(LockFreeProtocolStats::new()),
            session_id,
            connection_info,
            is_connected: std::sync::atomic::AtomicBool::new(true),
        };

        (adapter, event_rx)
    }

    /// [OPTIMIZATION] Core optimization 1: Non-blocking send
    pub fn send_nowait(&self, packet: Packet) -> Result<(), TransportError> {
        let start_time = Instant::now();

        let packet_size = packet.payload.len();
        let result = self
            .send_tx
            .try_send(packet)
            .map_err(|_| TransportError::connection_error("Channel full: protocol_send", false));

        if result.is_ok() {
            self.stats.record_send(packet_size);
            let _ = self
                .event_tx
                .try_send(ProtocolEvent::PacketSent(packet_size));
        } else {
            self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
        }

        let latency = start_time.elapsed().as_nanos() as u64;
        self.stats.record_operation_latency(latency);

        result
    }

    /// [OPTIMIZATION] Core optimization 2: Batch send processing
    pub async fn process_send_batch(&self, max_batch: usize) -> Result<usize, TransportError> {
        let start_time = Instant::now();
        let mut batch = Vec::with_capacity(max_batch);

        // Quickly collect batch data
        while batch.len() < max_batch {
            match self.send_rx.try_recv() {
                Ok(packet) => batch.push(packet),
                Err(_) => break,
            }
        }

        if batch.is_empty() {
            return Ok(0);
        }

        let batch_size = batch.len();
        let total_bytes: usize = batch.iter().map(|p| p.payload.len()).sum();

        // Simulate batch send processing (actual implementation would call underlying network interface)
        tokio::task::yield_now().await;

        // Update statistics
        self.stats.record_batch(batch_size);
        self.stats
            .bytes_sent
            .fetch_add(total_bytes as u64, Ordering::Relaxed);

        let latency = start_time.elapsed().as_nanos() as u64;
        self.stats.record_operation_latency(latency);

        let _ = self
            .event_tx
            .try_send(ProtocolEvent::BatchProcessed(batch_size));

        Ok(batch_size)
    }

    /// [OPTIMIZATION] Core optimization 3: Batch receive processing  
    pub async fn process_receive_batch(
        &self,
        max_batch: usize,
    ) -> Result<Vec<Packet>, TransportError> {
        let start_time = Instant::now();
        let mut batch = Vec::with_capacity(max_batch);

        // Attempt batch receiving
        while batch.len() < max_batch {
            match self.recv_rx.try_recv() {
                Ok(packet) => {
                    let packet_size = packet.payload.len();
                    self.stats.record_receive(packet_size);
                    batch.push(packet);
                }
                Err(_) => break,
            }
        }

        if !batch.is_empty() {
            self.stats.record_batch(batch.len());
            let _ = self
                .event_tx
                .try_send(ProtocolEvent::BatchProcessed(batch.len()));
        }

        let latency = start_time.elapsed().as_nanos() as u64;
        self.stats.record_operation_latency(latency);

        Ok(batch)
    }

    /// Simulate packet reception (for testing)
    pub fn simulate_receive(&self, packet: Packet) -> Result<(), TransportError> {
        self.recv_tx
            .try_send(packet)
            .map_err(|_| TransportError::connection_error("Channel full: protocol_recv", false))
    }

    /// High-performance statistics query
    pub fn get_stats(&self) -> ProtocolStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get real-time performance metrics
    pub fn get_performance_metrics(&self) -> PerformanceMetrics {
        let stats = self.stats.snapshot();

        PerformanceMetrics {
            throughput_pps: if stats.operation_count > 0 {
                stats.operation_count as f64
            } else {
                0.0
            },
            average_latency_ns: stats.average_latency_ns(),
            average_batch_size: stats.average_batch_size(),
            cache_hit_rate: stats.cache_hit_rate(),
            error_rate: if stats.operation_count > 0 {
                (stats.send_errors + stats.receive_errors) as f64 / stats.operation_count as f64
            } else {
                0.0
            },
        }
    }

    /// Session management interface
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn set_session_id(&mut self, session_id: SessionId) {
        self.session_id = session_id;
    }

    pub fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Relaxed)
    }

    pub fn set_connected(&self, connected: bool) {
        self.is_connected.store(connected, Ordering::Relaxed);
        let _ = self
            .event_tx
            .try_send(ProtocolEvent::ConnectionStatus(connected));
    }
}

/// Performance metrics
#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    pub throughput_pps: f64,     // Packets per second
    pub average_latency_ns: f64, // Average latency (nanoseconds)
    pub average_batch_size: f64, // Average batch size
    pub cache_hit_rate: f64,     // Cache hit rate
    pub error_rate: f64,         // Error rate
}

impl PerformanceMetrics {
    pub fn latency_us(&self) -> f64 {
        self.average_latency_ns / 1000.0
    }

    pub fn latency_ms(&self) -> f64 {
        self.average_latency_ns / 1_000_000.0
    }
}

/// Utility function: Create test packet
pub fn create_test_packet(id: u64, size: usize) -> Packet {
    Packet::one_way(id as u32, vec![0u8; size])
}

impl Clone for FlumePoweredProtocolAdapter {
    fn clone(&self) -> Self {
        Self {
            send_tx: self.send_tx.clone(),
            send_rx: self.send_rx.clone(),
            recv_tx: self.recv_tx.clone(),
            recv_rx: self.recv_rx.clone(),
            event_tx: self.event_tx.clone(),
            stats: self.stats.clone(),
            session_id: self.session_id,
            connection_info: self.connection_info.clone(),
            is_connected: std::sync::atomic::AtomicBool::new(
                self.is_connected.load(std::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

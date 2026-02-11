use crate::{
    command::TransportCommand, error::TransportError, packet::Packet,
    protocol::adapter::ProtocolAdapter, SessionId,
};
use flume::{unbounded as flume_unbounded, Receiver as FlumeReceiver, Sender as FlumeSender};
/// Frontend Actor layer complete migration
///
/// Core optimizations:
/// 1. Real network adapter integration
/// 2. Data pipeline and command pipeline separation  
/// 3. Batch data processing
/// 4. Flume high-performance communication
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info};

/// Optimized Actor event types
#[derive(Debug, Clone)]
pub enum ActorEvent {
    /// Connection established
    Connected,
    /// Packet sent completed
    PacketSent { packet_id: u32, size: usize },
    /// Packet received completed  
    PacketReceived { packet_id: u32, size: usize },
    /// Batch processing completed
    BatchCompleted(usize),
    /// Error occurred
    Error { message: String },
    /// Actor shutdown
    Shutdown,
    /// Health check
    HealthCheck,
}

/// Optimized Actor command types
#[derive(Debug, Clone)]
pub enum ActorCommand {
    /// Send packet
    SendPacket(Packet),
    /// Get statistics
    GetStats,
    /// Health check
    HealthCheck,
    /// Shutdown Actor
    Shutdown,
}

/// Lock-free Actor statistics
#[derive(Debug, Default)]
pub struct LockFreeActorStats {
    /// Number of packets sent
    pub packets_sent: AtomicU64,
    /// Number of packets received
    pub packets_received: AtomicU64,
    /// Bytes sent
    pub bytes_sent: AtomicU64,
    /// Bytes received
    pub bytes_received: AtomicU64,
    /// Batch operation count
    pub batch_operations: AtomicU64,
    /// Total packets processed (batch)
    pub total_batch_packets: AtomicU64,
    /// Error count
    pub error_count: AtomicU64,
    /// Start time
    pub started_at: std::sync::Mutex<Option<Instant>>,
}

/// Actor statistics snapshot
pub type ActorStats = LockFreeActorStats;

impl LockFreeActorStats {
    /// Create new statistics instance
    pub fn new() -> Self {
        Self {
            started_at: std::sync::Mutex::new(Some(Instant::now())),
            ..Default::default()
        }
    }

    /// Record packet sent
    pub fn record_packet_sent(&self, size: usize) {
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(size as u64, Ordering::Relaxed);
    }

    /// Record packet received
    pub fn record_packet_received(&self, size: usize) {
        self.packets_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(size as u64, Ordering::Relaxed);
    }

    /// Record batch operation
    pub fn record_batch_operation(&self, packet_count: usize) {
        self.batch_operations.fetch_add(1, Ordering::Relaxed);
        self.total_batch_packets
            .fetch_add(packet_count as u64, Ordering::Relaxed);
    }

    /// Record error
    pub fn record_error(&self) {
        self.error_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Average batch size
    pub fn average_batch_size(&self) -> f64 {
        let batch_ops = self.batch_operations.load(Ordering::Relaxed);
        if batch_ops == 0 {
            0.0
        } else {
            self.total_batch_packets.load(Ordering::Relaxed) as f64 / batch_ops as f64
        }
    }

    /// Get uptime in seconds
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0)
    }
}

/// Optimized Actor implementation - real network adapter integration
pub struct OptimizedActor<A: ProtocolAdapter> {
    /// Session ID
    session_id: SessionId,

    /// Command receive channel (compatible with traditional mpsc)
    command_receiver: mpsc::Receiver<TransportCommand>,

    /// Event send channel
    event_sender: FlumeSender<ActorEvent>,

    /// Internal high-performance data processing channel
    data_sender: FlumeSender<Packet>,
    data_receiver: FlumeReceiver<Packet>,

    /// Internal high-performance command processing channel  
    internal_command_sender: FlumeSender<ActorCommand>,
    internal_command_receiver: FlumeReceiver<ActorCommand>,

    /// Real protocol adapter (shared using Arc<Mutex<>>)
    protocol_adapter: Arc<Mutex<A>>,

    /// Performance statistics
    stats: Arc<LockFreeActorStats>,

    /// Batch processing configuration
    max_batch_size: usize,

    /// Global event sender (compatible with existing system)
    global_event_sender: tokio::sync::broadcast::Sender<crate::TransportEvent>,
}

impl<A: ProtocolAdapter> OptimizedActor<A> {
    /// Create new optimized Actor (integrated with real network adapter)
    pub fn new_with_real_adapter(
        session_id: SessionId,
        protocol_adapter: A,
        max_batch_size: usize,
        global_event_sender: tokio::sync::broadcast::Sender<crate::TransportEvent>,
    ) -> (
        Self,
        FlumeReceiver<ActorEvent>,
        FlumeSender<Packet>,
        mpsc::Sender<TransportCommand>,
    ) {
        let (event_sender, event_receiver) = flume_unbounded();
        let (data_sender, data_receiver) = flume_unbounded();
        let (internal_command_sender, internal_command_receiver) = flume_unbounded();
        let (command_sender, command_receiver) = mpsc::channel(1024);

        let stats = Arc::new(LockFreeActorStats::new());

        let actor = Self {
            session_id,
            command_receiver,
            event_sender,
            data_sender: data_sender.clone(),
            data_receiver,
            internal_command_sender,
            internal_command_receiver,
            protocol_adapter: Arc::new(Mutex::new(protocol_adapter)),
            stats,
            max_batch_size,
            global_event_sender,
        };

        (actor, event_receiver, data_sender, command_sender)
    }

    /// Run optimized dual pipeline processing - real network adapter version
    pub async fn run_dual_pipeline(self) -> Result<(), TransportError>
    where
        A: Send + 'static,
        A::Config: Send + 'static,
    {
        info!(
            "[PERF] Starting optimized Actor dual pipeline processing (session: {})",
            self.session_id
        );

        // Start command adapter task
        let internal_cmd_sender = self.internal_command_sender.clone();
        let mut command_receiver = self.command_receiver;
        let session_id = self.session_id;

        let cmd_adapter_task = tokio::spawn(async move {
            info!(
                "[CONTROL] Starting command adapter (session: {})",
                session_id
            );
            while let Some(transport_cmd) = command_receiver.recv().await {
                match transport_cmd {
                    TransportCommand::Send {
                        session_id: cmd_session_id,
                        packet,
                        response_tx,
                    } => {
                        if cmd_session_id != session_id {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Invalid session",
                                    false,
                                ),
                            ));
                            continue;
                        }

                        // Send to internal data processing pipeline
                        if let Err(_) = internal_cmd_sender.send(ActorCommand::SendPacket(packet)) {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Internal channel closed",
                                    false,
                                ),
                            ));
                            break;
                        } else {
                            // Send success, respond immediately (data will be processed asynchronously in data processing pipeline)
                            let _ = response_tx.send(Ok(()));
                        }
                    }
                    TransportCommand::Close {
                        session_id: cmd_session_id,
                        response_tx,
                    } => {
                        if cmd_session_id != session_id {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Invalid session",
                                    false,
                                ),
                            ));
                            continue;
                        }

                        if let Err(_) = internal_cmd_sender.send(ActorCommand::Shutdown) {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Internal channel closed",
                                    false,
                                ),
                            ));
                        } else {
                            let _ = response_tx.send(Ok(()));
                        }
                        break;
                    }
                    TransportCommand::GetStats { response_tx } => {
                        // Return current statistics
                        let stats = crate::command::TransportStats::default(); // TODO: Convert from actual stats
                        let _ = response_tx.send(stats);
                    }
                    TransportCommand::GetConnectionInfo {
                        session_id: cmd_session_id,
                        response_tx,
                    } => {
                        if cmd_session_id != session_id {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Invalid session",
                                    false,
                                ),
                            ));
                            continue;
                        }

                        // TODO: Return actual connection info
                        let info = crate::command::ConnectionInfo::default();
                        let _ = response_tx.send(Ok(info));
                    }
                    TransportCommand::Configure { .. } => {
                        debug!("[CONTROL] Configuration command not yet supported");
                    }
                    TransportCommand::GetActiveSessions { response_tx } => {
                        // Return current session
                        let _ = response_tx.send(vec![session_id]);
                    }
                    TransportCommand::ForceDisconnect {
                        session_id: cmd_session_id,
                        reason: _,
                        response_tx,
                    } => {
                        if cmd_session_id != session_id {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Invalid session",
                                    false,
                                ),
                            ));
                            continue;
                        }

                        if let Err(_) = internal_cmd_sender.send(ActorCommand::Shutdown) {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Internal channel closed",
                                    false,
                                ),
                            ));
                        } else {
                            let _ = response_tx.send(Ok(()));
                        }
                        break;
                    }
                    TransportCommand::PauseSession {
                        session_id: cmd_session_id,
                        response_tx,
                    } => {
                        if cmd_session_id != session_id {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Invalid session",
                                    false,
                                ),
                            ));
                            continue;
                        }

                        debug!("[CONTROL] Pause session command not yet supported");
                        let _ = response_tx.send(Ok(()));
                    }
                    TransportCommand::ResumeSession {
                        session_id: cmd_session_id,
                        response_tx,
                    } => {
                        if cmd_session_id != session_id {
                            let _ = response_tx.send(Err(
                                crate::error::TransportError::connection_error(
                                    "Invalid session",
                                    false,
                                ),
                            ));
                            continue;
                        }

                        debug!("[CONTROL] Resume session command not yet supported");
                        let _ = response_tx.send(Ok(()));
                    }
                }
            }
            info!("[CONTROL] Command adapter exited (session: {})", session_id);
        });

        // Start data processing pipeline
        let data_receiver = self.data_receiver.clone();
        let stats = self.stats.clone();
        let max_batch_size = self.max_batch_size;
        let protocol_adapter = self.protocol_adapter.clone(); // Clone Arc
        let event_sender = self.event_sender.clone();
        let global_event_sender = self.global_event_sender.clone();
        let _session_id = self.session_id;

        let data_task = tokio::spawn(async move {
            info!(
                "[DATA] Starting data processing pipeline (max batch: {})",
                max_batch_size
            );
            let mut batch = Vec::with_capacity(max_batch_size);

            loop {
                // Try to collect a batch of packets
                match data_receiver.recv_async().await {
                    Ok(packet) => {
                        batch.push(packet);

                        // Try to collect more packets into batch
                        while batch.len() < max_batch_size {
                            match data_receiver.try_recv() {
                                Ok(packet) => batch.push(packet),
                                Err(_) => break, // No more packets, process current batch
                            }
                        }

                        // Process batch
                        let batch_size = batch.len();
                        debug!("[DATA] Processing packet batch: {} packets", batch_size);

                        let mut should_break = false;
                        for packet in batch.drain(..) {
                            // [CONFIG] Acquire lock within task and send
                            {
                                let mut adapter = protocol_adapter.lock().await;

                                // [CHECK] Check connection status - if connection is closed, exit entire data processing loop
                                if !adapter.is_connected() {
                                    debug!("[SEND] Connection closed, stopping data processing pipeline");
                                    should_break = true;
                                    break; // Exit batch processing loop
                                }

                                debug!("[SEND] Sending packet: {} bytes", packet.payload.len());
                                match adapter.send(packet.clone()).await {
                                    Ok(_) => {
                                        debug!(
                                            "[SEND] Send successful: {} bytes",
                                            packet.payload.len()
                                        );
                                        stats.record_packet_sent(packet.payload.len());

                                        // Send global event (compatible with existing system)
                                        let transport_event = crate::TransportEvent::MessageSent {
                                            packet_id: packet.header.message_id,
                                        };
                                        let _ = global_event_sender.send(transport_event);
                                    }
                                    Err(e) => {
                                        // Simplified error handling, avoid duplicate logs
                                        debug!(
                                            "[SEND] Send failed (connection may be closed): {:?}",
                                            e
                                        );
                                        stats.record_error();

                                        // If connection close error, stop processing
                                        if !adapter.is_connected() {
                                            debug!("[SEND] Connection closed, stopping data processing pipeline");
                                            should_break = true;
                                            break;
                                        }

                                        // Send error event
                                        let transport_event =
                                            crate::TransportEvent::TransportError {
                                                error: TransportError::connection_error(
                                                    format!("{:?}", e),
                                                    false,
                                                ),
                                            };
                                        let _ = global_event_sender.send(transport_event);
                                    }
                                }
                            }
                        }

                        // If connection is closed, exit main loop
                        if should_break {
                            break;
                        }

                        stats.record_batch_operation(batch_size);
                        let _ = event_sender.send(ActorEvent::BatchCompleted(batch_size));
                    }
                    Err(_) => {
                        debug!("[DATA] Data processing pipeline: receive channel closed");
                        break;
                    }
                }
            }

            info!("[DATA] Data processing pipeline exited");
            Ok::<(), TransportError>(())
        });

        // Start receive processing pipeline
        let _stats = self.stats.clone();
        let _event_sender = self.event_sender.clone();
        let _global_event_sender = self.global_event_sender.clone();
        let protocol_adapter = self.protocol_adapter.clone(); // Clone Arc for receive
        let _session_id = self.session_id;

        let recv_task = tokio::spawn(async move {
            info!("[RECV] Starting event-driven receive pipeline");

            // [CONFIG] In event-driven architecture, we no longer call receive() directly
            // but receive data through protocol adapter's event stream
            // Here we just wait, actual data reception is handled by adapter's internal event loop

            // Simulate event-driven receive processing
            loop {
                // In real event-driven implementation, this should listen to event stream
                // Currently as placeholder, waiting for event-driven architecture complete implementation
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                // Check connection status
                let is_connected = {
                    let adapter = protocol_adapter.lock().await;
                    adapter.is_connected()
                };

                if !is_connected {
                    info!("[RECV] Connection disconnected, exiting event-driven receive pipeline");
                    break;
                }
            }

            info!("[RECV] Event-driven receive pipeline exited");
            Ok::<(), TransportError>(())
        });

        // Start command processing pipeline
        let internal_command_receiver = self.internal_command_receiver;
        let event_sender = self.event_sender.clone();
        let global_event_sender = self.global_event_sender.clone();
        let data_sender = self.data_sender.clone(); // [CONFIG] Fix: clone data_sender
        let _session_id = self.session_id;

        let command_task = tokio::spawn(async move {
            info!("[CONTROL] Starting command processing pipeline");

            while let Ok(command) = internal_command_receiver.recv_async().await {
                match command {
                    ActorCommand::SendPacket(packet) => {
                        // Send packet to data processing pipeline
                        if let Err(_) = data_sender.send(packet) {
                            error!("[CONTROL] Cannot send to data pipeline: channel closed");
                            break;
                        }
                    }
                    ActorCommand::GetStats => {
                        debug!("[CONTROL] Processing statistics query");
                        // Can return statistics via event
                    }
                    ActorCommand::Shutdown => {
                        info!("[STOP] Received shutdown command");
                        let _ = event_sender.send(ActorEvent::Shutdown);

                        // Send global shutdown event
                        let transport_event = crate::TransportEvent::ConnectionClosed {
                            reason: crate::CloseReason::Normal,
                        };
                        let _ = global_event_sender.send(transport_event);
                        break;
                    }
                    ActorCommand::HealthCheck => {
                        debug!("[HEALTH] Health check");
                        let _ = event_sender.send(ActorEvent::HealthCheck);
                    }
                }
            }

            info!("[CONTROL] Command processing pipeline exited");
            Ok::<(), TransportError>(())
        });

        // Wait for all tasks to complete
        let (cmd_adapter_result, data_result, recv_result, command_result) =
            tokio::join!(cmd_adapter_task, data_task, recv_task, command_task);

        match (cmd_adapter_result, data_result, recv_result, command_result) {
            (Ok(()), Ok(Ok(())), Ok(Ok(())), Ok(Ok(()))) => {
                info!(
                    "[SUCCESS] Optimized Actor normal exit (session: {})",
                    self.session_id
                );
                Ok(())
            }
            (cmd_res, data_res, recv_res, cmd_pipeline_res) => {
                error!("[ERROR] Optimized Actor abnormal exit (session: {}): cmd_adapter={:?}, data={:?}, recv={:?}, cmd_pipeline={:?}", 
                       self.session_id, cmd_res, data_res, recv_res, cmd_pipeline_res);
                Err(TransportError::connection_error(
                    "Actor pipeline failed",
                    false,
                ))
            }
        }
    }

    /// [CONFIG] Compatibility method: simulate traditional Actor running
    pub async fn run_flume_pipeline(self) -> Result<(), TransportError> {
        self.run_dual_pipeline().await
    }

    /// Get statistics
    pub fn get_stats(&self) -> Arc<LockFreeActorStats> {
        self.stats.clone()
    }
}

/// Type-erased Actor manager
pub struct ActorManager {
    /// Use dynamic dispatch to manage different types of Actors
    actor_handles: Vec<tokio::task::JoinHandle<Result<(), TransportError>>>,
    stats: Arc<LockFreeActorStats>,
}

impl ActorManager {
    /// Create new ActorManager
    pub fn new() -> Self {
        Self {
            actor_handles: Vec::new(),
            stats: Arc::new(LockFreeActorStats::new()),
        }
    }

    /// Add Actor (start and manage)
    pub fn add_actor<A>(&mut self, actor: OptimizedActor<A>)
    where
        A: ProtocolAdapter + Send + 'static,
        A::Config: Send + 'static,
    {
        let handle = tokio::spawn(async move { actor.run_dual_pipeline().await });
        self.actor_handles.push(handle);
    }

    /// Run all Actors concurrently
    pub async fn run_all(self) -> Result<(), TransportError> {
        let results = futures::future::join_all(self.actor_handles).await;

        for (index, result) in results.into_iter().enumerate() {
            match result {
                Ok(Ok(())) => {
                    info!("[SUCCESS] Actor {} completed normally", index);
                }
                Ok(Err(e)) => {
                    error!("[ERROR] Actor {} runtime error: {:?}", index, e);
                }
                Err(e) => {
                    error!("[ERROR] Actor {} task error: {:?}", index, e);
                }
            }
        }

        info!("[COMPLETE] All Actors completed");
        Ok(())
    }
}

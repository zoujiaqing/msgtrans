use bytes::Bytes;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc,
};
/// Client transport layer module
///
/// Provides transport layer API specifically designed for client connections
use std::time::Duration;
use tokio::{sync::RwLock, task::JoinHandle};

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

/// Client transport layer builder
pub struct TransportClientBuilder {
    retry_config: RetryConfig,
    transport_config: TransportConfig,
    /// Protocol configuration storage - Client only supports one protocol connection
    protocol_config: Option<Box<dyn DynClientConfig>>,
    /// Frame decode policy applied to the connection.
    frame_policy: crate::packet::FramePolicy,
}

impl TransportClientBuilder {
    pub fn new() -> Self {
        Self {
            retry_config: RetryConfig::default(),
            transport_config: TransportConfig::default(),
            protocol_config: None,
            frame_policy: crate::packet::FramePolicy::default(),
        }
    }

    /// Set protocol configuration - Client specific
    pub fn protocol<T: DynClientConfig>(mut self, config: T) -> Self {
        self.protocol_config = Some(Box::new(config));
        self
    }

    /// Set the frame decode policy applied to the connection (default Lenient).
    ///
    /// Under `Strict`, an undecodable frame from the server closes the connection
    /// instead of being downgraded to a raw one-way message.
    pub fn frame_policy(mut self, policy: crate::packet::FramePolicy) -> Self {
        self.frame_policy = policy;
        self
    }

    /// Client specific: Retry strategy
    pub fn retry_strategy(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// Set transport layer basic configuration
    pub fn transport_config(mut self, config: TransportConfig) -> Self {
        self.transport_config = config;
        self
    }

    /// Build client transport layer - return TransportClient
    pub async fn build(self) -> Result<TransportClient, TransportError> {
        let ctx = crate::transport::context::TransportContext::new().await?;
        let transport = Transport::with_context(self.transport_config, &ctx);

        Ok(TransportClient::new(
            transport,
            self.retry_config,
            self.protocol_config,
            self.frame_policy,
        ))
    }
}

impl Default for TransportClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// [TARGET] Transport layer client - Uses Transport for single connection management
/// Dropping a TransportClient deterministically REQUESTS cancellation (the
/// abort cannot be skipped) but does not join the task; resources are then
/// released by cascade — background tasks hold only Weak<Transport>, so the
/// Transport, connection, socket and the server-side session/permit are freed
/// shortly after, without an explicit disconnect(). Completion guarantees
/// belong to [`Self::shutdown`], which joins the owned teardown task.
pub struct TransportClient {
    inner: Arc<Transport>,
    retry_config: RetryConfig,
    // Client protocol configuration
    protocol_config: Option<Box<dyn DynClientConfig>>,
    frame_policy: crate::packet::FramePolicy,
    // [TARGET] Current connection session ID - Uses Arc<RwLock> for modification
    current_session_id: Arc<RwLock<Option<SessionId>>>,
    /// Client events go to a single consumer over a bounded channel: a client
    /// has exactly one connection, so there is nothing to fan out to, and a
    /// bounded queue means a slow consumer is throttled rather than silently
    /// skipped the way a broadcast lag would skip it.
    event_sender: tokio::sync::mpsc::Sender<crate::event::ClientEvent>,
    event_receiver: Arc<RwLock<Option<tokio::sync::mpsc::Receiver<crate::event::ClientEvent>>>>,
    event_forwarding_running: Arc<AtomicBool>,
    event_forwarding_task: Arc<RwLock<Option<JoinHandle<()>>>>,
    /// Synchronously accessible abort handle for Drop (try_write on the
    /// RwLock could silently skip the abort under contention).
    forwarding_abort: Arc<std::sync::Mutex<Option<tokio::task::AbortHandle>>>,
    /// Client lifecycle: 0 Active, 1 ShuttingDown, 2 Stopped. Once Stopped,
    /// connect() rejects before touching the network — the take-once event
    /// receiver is gone, so a new connection could never receive events and
    /// would be a half-alive lie.
    phase: Arc<AtomicU8>,
    /// The owned teardown task (spawned once, handle stored HERE — never in a
    /// cancellable public future). shutdown() only observes its completion
    /// watch, so cancelling shutdown() loses nothing.
    teardown_task: Arc<std::sync::Mutex<Option<JoinHandle<()>>>>,
    teardown_done: Arc<tokio::sync::watch::Sender<bool>>,
    teardown_err: Arc<std::sync::Mutex<Option<TransportError>>>,
}

const CLIENT_ACTIVE: u8 = 0;
const CLIENT_SHUTTING_DOWN: u8 = 1;
const CLIENT_STOPPED: u8 = 2;

/// Capacity of the client event queue.
const CLIENT_EVENT_QUEUE: usize = 8192;

impl TransportClient {
    pub(crate) fn new(
        transport: Transport,
        retry_config: RetryConfig,
        protocol_config: Option<Box<dyn DynClientConfig>>,
        frame_policy: crate::packet::FramePolicy,
    ) -> Self {
        let (event_sender, event_receiver) = tokio::sync::mpsc::channel(CLIENT_EVENT_QUEUE);
        Self {
            inner: Arc::new(transport),
            retry_config,
            protocol_config,
            frame_policy,
            current_session_id: Arc::new(RwLock::new(None)),
            event_sender,
            event_receiver: Arc::new(RwLock::new(Some(event_receiver))),
            event_forwarding_running: Arc::new(AtomicBool::new(false)),
            event_forwarding_task: Arc::new(RwLock::new(None)),
            forwarding_abort: Arc::new(std::sync::Mutex::new(None)),
            phase: Arc::new(AtomicU8::new(CLIENT_ACTIVE)),
            teardown_task: Arc::new(std::sync::Mutex::new(None)),
            teardown_done: Arc::new(tokio::sync::watch::channel(false).0),
            teardown_err: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// [CONNECT] Use protocol configuration specified at build time for connection - Framework's only connection method
    pub async fn connect(&mut self) -> Result<(), TransportError> {
        // Lifecycle gate BEFORE any network work: a shut-down client's event
        // receiver is permanently gone, so a connection made here could send
        // but never receive — reject instead of building that half-alive state.
        if self.phase.load(Ordering::SeqCst) != CLIENT_ACTIVE {
            return Err(TransportError::connection_error(
                "Client has been shut down; create a new client to reconnect",
                false,
            ));
        }
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

        // Apply the configured frame policy to the freshly-established connection.
        self.inner.set_frame_policy(self.frame_policy).await;

        // Update current session ID (internal use)
        let mut current_session = self.current_session_id.write().await;
        *current_session = Some(session_id);
        drop(current_session);

        // [START] Start event forwarding task (CAS-guarded: first connect
        // spawns it, reconnects reuse it). It owns the Transport's single
        // client-event receiver, so it must survive disconnect/reconnect —
        // aborting it would strand the take-once receiver and make every
        // later connect() unable to receive events.
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
                #[cfg(feature = "tcp")]
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
                #[cfg(feature = "websocket")]
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
                #[cfg(feature = "quic")]
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

    /// Terminate this client's lifecycle: disconnect (closing this
    /// generation's pending requests), then cancel AND JOIN the forwarding
    /// task — the completion guarantee Drop deliberately does not make.
    ///
    /// `disconnect()` is the reconnectable operation (the client can
    /// `connect()` again); `shutdown()` ends the client. After it returns, no
    /// background task of this client is running.
    pub async fn shutdown(&mut self) -> Result<(), TransportError> {
        // Lifecycle gate: Active starts the teardown; ShuttingDown (including
        // a shutdown future that was cancelled mid-flight) resumes by
        // OBSERVING the owned task; Stopped is done.
        loop {
            match self.phase.load(Ordering::SeqCst) {
                CLIENT_STOPPED => break,
                CLIENT_SHUTTING_DOWN => break,
                _ => {
                    if self
                        .phase
                        .compare_exchange(
                            CLIENT_ACTIVE,
                            CLIENT_SHUTTING_DOWN,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
            }
        }

        // Spawn the owned teardown exactly once. The task captures only Arcs;
        // its handle lives in the field, so no caller cancellation can drop
        // it. All the destructive work happens inside it.
        {
            let mut slot = self.teardown_task.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() && self.phase.load(Ordering::SeqCst) != CLIENT_STOPPED {
                let transport = self.inner.clone();
                let forwarding = self.event_forwarding_task.clone();
                let abort_slot = self.forwarding_abort.clone();
                let running = self.event_forwarding_running.clone();
                let phase = self.phase.clone();
                let done = self.teardown_done.clone();
                let err_slot = self.teardown_err.clone();
                *slot = Some(tokio::spawn(async move {
                    // Completion guard: even if any teardown step below
                    // PANICS, the phase still reaches Stopped and the watch
                    // still fires — shutdown() observers can never wait
                    // forever on a dead task.
                    struct TeardownDone {
                        phase: Arc<AtomicU8>,
                        done: Arc<tokio::sync::watch::Sender<bool>>,
                    }
                    impl Drop for TeardownDone {
                        fn drop(&mut self) {
                            self.phase.store(CLIENT_STOPPED, Ordering::SeqCst);
                            let _ = self.done.send_replace(true);
                        }
                    }
                    let _done_guard = TeardownDone {
                        phase: phase.clone(),
                        done: done.clone(),
                    };
                    let was_connected = transport.current_session_id().await.is_some();
                    if let Err(e) = transport.disconnect().await {
                        if was_connected {
                            *err_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                        }
                    }
                    let handle = forwarding.write().await.take();
                    if let Some(handle) = handle {
                        handle.abort();
                        let _ = handle.await; // abort guarantees completion
                    }
                    abort_slot.lock().unwrap_or_else(|p| p.into_inner()).take();
                    running.store(false, Ordering::SeqCst);
                    phase.store(CLIENT_STOPPED, Ordering::SeqCst);
                    let _ = done.send_replace(true);
                }));
            }
        }

        // Observe completion (repeatable and cancel-safe: this is a watch,
        // not the task handle).
        let mut done_rx = self.teardown_done.subscribe();
        while !*done_rx.borrow() {
            if done_rx.changed().await.is_err() {
                break;
            }
        }
        // Formal join, cancellation-proof: the handle is only TAKEN once it is
        // already finished, so a cancellation landing on the await below can
        // at worst drop a completed handle — which loses nothing. Until it is
        // finished, ownership stays in the field for the next caller.
        loop {
            let finished = {
                let slot = self.teardown_task.lock().unwrap_or_else(|e| e.into_inner());
                match slot.as_ref() {
                    Some(handle) => handle.is_finished(),
                    None => break, // someone else already joined it
                }
            };
            if finished {
                let handle = self
                    .teardown_task
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if let Some(handle) = handle {
                    // The completion guard woke us; the JOIN reports what
                    // actually happened — a panicked teardown must not be
                    // presented as a successful shutdown.
                    if handle.await.is_err() {
                        return Err(TransportError::connection_error(
                            "client teardown panicked",
                            false,
                        ));
                    }
                }
                break;
            }
            tokio::task::yield_now().await;
        }
        match self
            .teardown_err
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// [DISCONNECT] Disconnect (graceful close)
    ///
    /// Reconnectable: closes the current connection and this generation's
    /// pending requests, but leaves the client (and its forwarding task)
    /// ready for a new `connect()`. To end the client entirely, use
    /// [`Self::shutdown`].
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

        let message_id = self.inner.next_message_id();
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

        let message_id = self.inner.next_message_id();
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
                if matches!(e, TransportError::Timeout { .. }) {
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

    /// Take this client's event stream.
    ///
    /// A client owns a single connection, so the stream has a single consumer
    /// and can only be taken once; a second call returns an error. Take it
    /// *before* `connect()` so no event is missed, and keep consuming it —
    /// the queue is bounded, so a stalled consumer backpressures the
    /// connection instead of losing events.
    pub async fn events(&self) -> Result<crate::stream::ClientEvents, TransportError> {
        match self.event_receiver.write().await.take() {
            Some(rx) => Ok(crate::stream::ClientEvents::new(rx)),
            None => Err(TransportError::connection_error(
                "Client event stream already taken - it has a single consumer",
                false,
            )),
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

    /// [START] Start event forwarding task
    async fn start_event_forwarding(&self) -> Result<(), TransportError> {
        if self
            .event_forwarding_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            tracing::debug!("[SKIP] Event forwarding task already running");
            return Ok(());
        }

        // Get Transport's event stream
        if let Some(mut transport_events) = self.inner.get_event_stream().await {
            let client_event_sender = self.event_sender.clone();
            // Weak: the forwarding task must not keep the Transport (and thus
            // the socket/session) alive after the client is dropped.
            let transport_for_response = Arc::downgrade(&self.inner);
            let forwarding_running = self.event_forwarding_running.clone();

            // Start forwarding task
            let handle = tokio::spawn(async move {
                tracing::debug!("[LOOP] TransportClient event forwarding task started");

                while let Some((source_session, transport_event)) = transport_events.recv().await {
                    tracing::debug!("[RECV] TransportClient received Transport event");

                    // [TARGET] Special handling of Request packets in MessageReceived
                    match &transport_event {
                        crate::event::TransportEvent::MessageReceived(packet)
                            if packet.header.packet_type == crate::packet::PacketType::Request =>
                        {
                            // Create TransportContext with real response functionality for Request packets
                            let transport = transport_for_response.clone();
                            let message_id = packet.header.message_id;
                            let biz_type = packet.header.biz_type;
                            let registry = transport_for_response
                                .upgrade()
                                .map(|t| t.request_registry().clone());

                            let context = crate::event::TransportContext::new_request_with_registry(
                                Some(source_session),
                                message_id,
                                biz_type,
                                if packet.ext_header.is_empty() {
                                    None
                                } else {
                                    Some(packet.ext_header.clone())
                                },
                                packet.payload.clone(),
                                Arc::new(
                                    move |response_data: bytes::Bytes,
                                          claim: Option<
                                        crate::transport::request_registry::RespondClaim,
                                    >|
                                          -> futures::future::BoxFuture<
                                        'static,
                                        Result<(), crate::error::TransportError>,
                                    > {
                                        let transport = transport.clone();
                                        Box::pin(async move {
                                            // Upgrade only for the send: the
                                            // responder must not keep the
                                            // Transport alive either. Dropping
                                            // `claim` on this path records the
                                            // send failure.
                                            let Some(transport) = transport.upgrade() else {
                                                return Err(
                                                    crate::error::TransportError::connection_error(
                                                        "Client dropped before response",
                                                        false,
                                                    ),
                                                );
                                            };
                                            let response_packet = crate::packet::Packet {
                                                header: crate::packet::FixedHeader {
                                                    version: 1,
                                                    compression:
                                                        crate::packet::CompressionType::None,
                                                    packet_type:
                                                        crate::packet::PacketType::Response,
                                                    biz_type,
                                                    message_id,
                                                    ext_header_len: 0,
                                                    payload_len: response_data.len() as u32,
                                                    reserved: crate::packet::ReservedFlags::new(),
                                                },
                                                ext_header: Vec::new(),
                                                payload: response_data,
                                            };
                                            // Generation-bound: if the client
                                            // reconnected since this request
                                            // arrived, the stale response is
                                            // refused instead of being written
                                            // onto the new connection.
                                            transport
                                                .send_confirmed_with(
                                                    response_packet,
                                                    Some(source_session),
                                                    claim,
                                                )
                                                .await
                                        })
                                    },
                                ),
                                registry,
                            );

                            let client_event = crate::event::ClientEvent::MessageReceived(context);
                            tracing::debug!(
                                "[SEND] TransportClient forwarding ClientEvent (Request): {:?}",
                                client_event
                            );

                            if client_event_sender.send(client_event).await.is_err() {
                                tracing::trace!(
                                    "[DROP] ClientEvents receiver gone; discarding event"
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

                                if client_event_sender.send(client_event).await.is_err() {
                                    tracing::trace!(
                                        "[DROP] ClientEvents receiver gone; discarding event"
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

                forwarding_running.store(false, Ordering::SeqCst);
                tracing::debug!("[END] TransportClient event forwarding task ended");
            });

            *self
                .forwarding_abort
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(handle.abort_handle());
            *self.event_forwarding_task.write().await = Some(handle);

            tracing::debug!("[SUCCESS] TransportClient event forwarding task started");
            Ok(())
        } else {
            self.event_forwarding_running.store(false, Ordering::SeqCst);
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

impl Drop for TransportClient {
    fn drop(&mut self) {
        // Best-effort cancellation, but never silently skipped: the
        // AbortHandle lives behind a sync Mutex, so no async-lock contention
        // can make Drop miss it. abort() requests cancellation; it does not
        // join — deterministic completion is the async shutdown path's job.
        if let Some(abort) = self
            .forwarding_abort
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            abort.abort();
        }
    }
}

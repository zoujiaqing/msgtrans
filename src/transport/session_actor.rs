//! Session Actor Module
//!
//! Implements per-connection actor model for high-throughput message processing.
//! Each connection gets its own dedicated queue and worker task, eliminating
//! global contention and enabling natural backpressure.
//!
//! Architecture:
//! ```text
//! Connection → flume(bounded) → SessionActor → SessionHandler
//!                     ↑ also receives Send/Close commands from TransportServer
//! ```

use crate::{event::TransportEvent, packet::Packet, transport::transport::Transport, SessionId};
use async_trait::async_trait;
use flume::{bounded, Receiver, Sender};
use std::sync::Arc;

/// Messages that flow through the actor's mailbox.
///
/// Combines inbound events (from the connection reader) and outbound commands
/// (from TransportServer's send path) into a single channel, eliminating the
/// need for the server to lock the connection Mutex on sends.
#[derive(Debug)]
pub enum ActorMessage {
    /// Inbound: a transport event from the connection reader task
    InboundEvent(TransportEvent),
    /// Outbound: send a packet through this actor's connection (fire-and-forget)
    Send(Packet),
    /// Outbound: send a packet and notify the caller of the result
    SendWithReply {
        packet: Packet,
        reply: tokio::sync::oneshot::Sender<Result<(), crate::TransportError>>,
    },
    /// Command: gracefully close the connection
    Close,
}

/// Session handler trait - business layer implements this to receive messages
///
/// This replaces the old `subscribe_events()` pattern with a direct callback model.
/// Each session has its own handler invocation, no global fan-out.
#[async_trait]
pub trait SessionHandler: Send + Sync + 'static {
    /// Called when a message is received from a session
    ///
    /// # Arguments
    /// * `session_id` - The session that sent the message
    /// * `packet` - The received packet
    /// * `sender` - Sender to send messages back to this session
    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender);

    /// Called when a session is established
    async fn on_connected(&self, session_id: SessionId) {
        let _ = session_id; // Default: no-op
    }

    /// Called when a session is closed
    async fn on_disconnected(&self, session_id: SessionId, reason: crate::error::CloseReason) {
        let _ = (session_id, reason); // Default: no-op
    }

    /// Called when a transport error occurs
    async fn on_error(&self, session_id: SessionId, error: crate::TransportError) {
        let _ = (session_id, error); // Default: no-op
    }
}

/// Sender for sending messages back to a session
///
/// This is provided to the handler for every message, allowing the handler
/// to send responses or other messages back to the session.
#[derive(Clone)]
pub struct SessionSender {
    session_id: SessionId,
    transport: Arc<Transport>,
}

impl SessionSender {
    pub(crate) fn new(session_id: SessionId, transport: Arc<Transport>) -> Self {
        Self {
            session_id,
            transport,
        }
    }

    /// Send a packet to this session
    pub async fn send(&self, packet: Packet) -> Result<(), crate::TransportError> {
        self.transport.send(packet).await
    }

    /// Send raw data to this session (one-way message)
    pub async fn send_data(&self, data: Vec<u8>) -> Result<(), crate::TransportError> {
        let packet = Packet::one_way(0, data);
        self.transport.send(packet).await
    }

    /// Send a response to a request
    pub async fn respond(
        &self,
        message_id: u32,
        biz_type: u8,
        data: Vec<u8>,
    ) -> Result<(), crate::TransportError> {
        let response_packet = Packet {
            header: crate::packet::FixedHeader {
                version: 1,
                compression: crate::packet::CompressionType::None,
                packet_type: crate::packet::PacketType::Response,
                biz_type,
                message_id,
                ext_header_len: 0,
                payload_len: data.len() as u32,
                reserved: crate::packet::ReservedFlags::new(),
            },
            ext_header: Vec::new(),
            payload: data,
        };
        self.transport.send(response_packet).await
    }

    /// Get the session ID
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
}

/// Responder for request-response pattern
///
/// When a request packet is received, the handler gets a Responder
/// that can be used to send the response back.
pub struct Responder {
    session_id: SessionId,
    message_id: u32,
    biz_type: u8,
    transport: Arc<Transport>,
    responded: std::sync::atomic::AtomicBool,
}

impl Responder {
    pub(crate) fn new(
        session_id: SessionId,
        message_id: u32,
        biz_type: u8,
        transport: Arc<Transport>,
    ) -> Self {
        Self {
            session_id,
            message_id,
            biz_type,
            transport,
            responded: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Send response data back to the client
    pub async fn respond(self, data: Vec<u8>) -> Result<(), crate::TransportError> {
        if self
            .responded
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(crate::TransportError::protocol_error(
                "session",
                "Already responded to this request",
            ));
        }

        let response_packet = Packet {
            header: crate::packet::FixedHeader {
                version: 1,
                compression: crate::packet::CompressionType::None,
                packet_type: crate::packet::PacketType::Response,
                biz_type: self.biz_type,
                message_id: self.message_id,
                ext_header_len: 0,
                payload_len: data.len() as u32,
                reserved: crate::packet::ReservedFlags::new(),
            },
            ext_header: Vec::new(),
            payload: data,
        };

        self.transport.send(response_packet).await
    }

    /// Get the session ID
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Get the request message ID
    pub fn message_id(&self) -> u32 {
        self.message_id
    }
}

/// Handle to a session actor, held by TransportServer
///
/// Contains only the sender side of the channel.
/// When this is dropped, the actor will shut down.
#[derive(Clone)]
pub struct SessionHandle {
    /// Channel to send messages (events + commands) to the actor
    pub(crate) tx: Sender<ActorMessage>,
    /// Reference to the transport (kept for backward compatibility / connection status checks)
    pub(crate) transport: Arc<Transport>,
}

impl SessionHandle {
    /// Forward an inbound transport event to the actor
    ///
    /// This will apply backpressure if the channel is full.
    pub async fn send_event(
        &self,
        event: TransportEvent,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<TransportEvent>> {
        self.tx
            .send_async(ActorMessage::InboundEvent(event))
            .await
            .map_err(|e| {
                // Extract the TransportEvent from the ActorMessage for the error
                match e.0 {
                    ActorMessage::InboundEvent(evt) => {
                        tokio::sync::mpsc::error::SendError(evt)
                    }
                    _ => unreachable!(),
                }
            })
    }

    /// Send a packet through the actor (fire-and-forget, no reply).
    pub async fn send_packet(&self, packet: Packet) -> Result<(), crate::TransportError> {
        self.tx
            .send_async(ActorMessage::Send(packet))
            .await
            .map_err(|_| crate::TransportError::connection_error("Actor channel closed", false))
    }

    /// Send a packet through the actor and wait for the send result.
    pub async fn send_packet_with_reply(
        &self,
        packet: Packet,
    ) -> Result<(), crate::TransportError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send_async(ActorMessage::SendWithReply {
                packet,
                reply: reply_tx,
            })
            .await
            .map_err(|_| crate::TransportError::connection_error("Actor channel closed", false))?;
        reply_rx.await.map_err(|_| {
            crate::TransportError::connection_error("Actor dropped before reply", false)
        })?
    }

    /// Get the transport for this session
    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }
}

/// Batch size for recv_many - optimal for reducing syscalls and scheduling overhead
const BATCH_SIZE: usize = 64;

/// Session Actor - processes events for a single connection
///
/// Each connection has exactly one SessionActor running in its own task.
/// The actor's mailbox receives both inbound events and outbound send commands,
/// ensuring all I/O for a connection is serialized through a single task.
///
/// Uses batch processing for high throughput:
/// - Reduces await/wake cycles by up to 64x
/// - Reduces channel lock contention
/// - Better CPU cache utilization
pub struct SessionActor {
    session_id: SessionId,
    transport: Arc<Transport>,
    rx: Receiver<ActorMessage>,
    handler: Arc<dyn SessionHandler>,
}

impl SessionActor {
    /// Create a new session actor
    pub fn new(
        session_id: SessionId,
        transport: Arc<Transport>,
        rx: Receiver<ActorMessage>,
        handler: Arc<dyn SessionHandler>,
    ) -> Self {
        Self {
            session_id,
            transport,
            rx,
            handler,
        }
    }

    /// Run the actor's event loop with batch processing
    ///
    /// Processes both inbound events (from the connection reader) and outbound
    /// commands (send requests from TransportServer). This eliminates the need
    /// for external callers to lock the connection Mutex.
    pub async fn run(self) {
        tracing::debug!(
            "[ACTOR] SessionActor started for session {}",
            self.session_id
        );

        // Notify handler that session is connected
        self.handler.on_connected(self.session_id).await;

        // Pre-allocate batch buffer to avoid repeated allocations
        let mut batch: Vec<ActorMessage> = Vec::with_capacity(BATCH_SIZE);

        // Create sender once, reuse for all messages (it's Clone + cheap)
        let sender = SessionSender::new(self.session_id, self.transport.clone());

        loop {
            batch.clear();

            // Batch receive: wait for first message, then drain up to BATCH_SIZE.
            match self.rx.recv_async().await {
                Ok(first) => batch.push(first),
                Err(_) => break,
            }
            while batch.len() < BATCH_SIZE {
                match self.rx.try_recv() {
                    Ok(next) => batch.push(next),
                    Err(flume::TryRecvError::Empty) => break,
                    Err(flume::TryRecvError::Disconnected) => break,
                }
            }

            // Process batch
            let mut should_break = false;
            for msg in batch.drain(..) {
                match msg {
                    ActorMessage::InboundEvent(event) => {
                        match event {
                            TransportEvent::MessageReceived(packet) => {
                                // Direct handler call - no spawn, no extra allocation
                                self.handler
                                    .on_message(self.session_id, packet, sender.clone())
                                    .await;
                            }
                            TransportEvent::MessageSent { packet_id } => {
                                tracing::trace!(
                                    "[ACTOR] Message {} sent for session {}",
                                    packet_id,
                                    self.session_id
                                );
                            }
                            TransportEvent::ConnectionClosed { reason } => {
                                tracing::debug!(
                                    "[ACTOR] Session {} closed: {:?}",
                                    self.session_id,
                                    reason
                                );
                                self.handler
                                    .on_disconnected(self.session_id, reason)
                                    .await;
                                should_break = true;
                            }
                            TransportEvent::TransportError { error } => {
                                tracing::warn!(
                                    "[ACTOR] Session {} error: {:?}",
                                    self.session_id,
                                    error
                                );
                                self.handler.on_error(self.session_id, error).await;
                            }
                            _ => {
                                tracing::trace!(
                                    "[ACTOR] Session {} received unhandled event",
                                    self.session_id
                                );
                            }
                        }
                    }
                    ActorMessage::Send(packet) => {
                        // Outbound send routed through actor — uses Transport::send
                        // which holds the Mutex internally, but now serialized per-connection.
                        if let Err(e) = self.transport.send(packet).await {
                            tracing::warn!(
                                "[ACTOR] Session {} send failed: {:?}",
                                self.session_id,
                                e
                            );
                        }
                    }
                    ActorMessage::SendWithReply { packet, reply } => {
                        let result = self.transport.send(packet).await;
                        let _ = reply.send(result);
                    }
                    ActorMessage::Close => {
                        tracing::debug!(
                            "[ACTOR] Session {} received close command",
                            self.session_id
                        );
                        should_break = true;
                    }
                }
            }

            if should_break {
                break;
            }
        }

        tracing::debug!(
            "[ACTOR] SessionActor stopped for session {}",
            self.session_id
        );
    }
}

/// Default buffer size for session actor channels
pub const DEFAULT_ACTOR_BUFFER_SIZE: usize = 2048;

/// Create a new session actor pair (handle + actor)
///
/// Returns the handle (for TransportServer) and the actor (to be spawned).
/// The channel carries `ActorMessage` — both inbound events and outbound commands.
pub fn create_session_actor(
    session_id: SessionId,
    transport: Arc<Transport>,
    handler: Arc<dyn SessionHandler>,
    buffer_size: usize,
) -> (SessionHandle, SessionActor) {
    let (tx, rx) = bounded(buffer_size);

    let handle = SessionHandle {
        tx,
        transport: transport.clone(),
    };

    let actor = SessionActor::new(session_id, transport, rx, handler);

    (handle, actor)
}

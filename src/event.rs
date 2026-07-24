use crate::command::ConnectionInfo;
use crate::error::TransportError;
use crate::packet::Packet;
use crate::transport::request_registry::{MarkResult, RequestRegistry};
use crate::{CloseReason, PacketId, SessionId};
use bytes::Bytes;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Unified abstraction for transport layer events
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// Connection related events
    ConnectionEstablished {
        info: ConnectionInfo,
    },
    ConnectionClosed {
        reason: CloseReason,
    },

    /// Data transmission events
    MessageReceived(Packet),
    MessageSent {
        packet_id: PacketId,
    },

    /// Error events
    TransportError {
        error: TransportError,
    },

    /// Server events
    ServerStarted {
        address: SocketAddr,
    },
    ServerStopped,

    /// Client events
    ClientConnected {
        address: SocketAddr,
    },
    ClientDisconnected,

    RequestReceived(RequestContext),
}

/// Protocol specific event trait
///
/// This trait allows each protocol to define its own specific event types while maintaining compatibility with the unified event system
pub trait ProtocolEvent: Clone + Send + std::fmt::Debug + 'static {
    /// Convert to generic transport event
    fn into_transport_event(self) -> TransportEvent;

    /// Get related session ID (if any)
    fn session_id(&self) -> Option<SessionId>;

    /// Check if it's a data transmission event
    fn is_data_event(&self) -> bool;

    /// Check if it's an error event
    fn is_error_event(&self) -> bool;
}

/// Simplified representation of connection events
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    Established {
        session_id: SessionId,
        info: ConnectionInfo,
    },
    Closed {
        session_id: SessionId,
        reason: CloseReason,
    },
}

impl TransportEvent {
    /// Get event related session ID
    pub fn session_id(&self) -> Option<SessionId> {
        match self {
            TransportEvent::ConnectionEstablished { .. } => None,
            TransportEvent::ConnectionClosed { .. } => None,
            TransportEvent::MessageReceived(..) => None,
            TransportEvent::MessageSent { .. } => None,
            TransportEvent::TransportError { .. } => None,
            TransportEvent::ServerStarted { .. } => None,
            TransportEvent::ServerStopped => None,
            TransportEvent::ClientConnected { .. } => None,
            TransportEvent::ClientDisconnected => None,
            TransportEvent::RequestReceived(context) => context.peer,
        }
    }

    /// Check if it's a connection related event
    pub fn is_connection_event(&self) -> bool {
        matches!(
            self,
            TransportEvent::ConnectionEstablished { .. } | TransportEvent::ConnectionClosed { .. }
        )
    }

    /// Check if it's a data transmission event
    pub fn is_data_event(&self) -> bool {
        matches!(
            self,
            TransportEvent::MessageReceived(..) | TransportEvent::RequestReceived(..)
        )
    }

    /// Check if it's an error event
    pub fn is_error_event(&self) -> bool {
        matches!(self, TransportEvent::TransportError { .. })
    }

    /// Check if it's a server event
    pub fn is_server_event(&self) -> bool {
        matches!(
            self,
            TransportEvent::ServerStarted { .. } | TransportEvent::ServerStopped
        )
    }

    /// Check if it's a client event
    pub fn is_client_event(&self) -> bool {
        matches!(
            self,
            TransportEvent::ClientConnected { .. } | TransportEvent::ClientDisconnected
        )
    }
}

/// TCP protocol specific events
#[cfg(feature = "tcp")]
#[derive(Debug, Clone)]
pub enum TcpEvent {
    ListenerBound { addr: SocketAddr },
    AcceptError { error: String },
    ConnectionTimeout { session_id: SessionId },
}

#[cfg(feature = "tcp")]
impl ProtocolEvent for TcpEvent {
    fn into_transport_event(self) -> TransportEvent {
        match self {
            TcpEvent::ListenerBound { addr } => TransportEvent::ServerStarted { address: addr },
            TcpEvent::AcceptError { error } => TransportEvent::TransportError {
                error: TransportError::connection_error(
                    format!("IO error: {:?}", std::io::Error::other(error)),
                    true,
                ),
            },
            TcpEvent::ConnectionTimeout { session_id } => TransportEvent::ConnectionClosed {
                reason: CloseReason::Timeout,
            },
        }
    }

    fn session_id(&self) -> Option<SessionId> {
        match self {
            TcpEvent::ConnectionTimeout { session_id } => Some(*session_id),
            _ => None,
        }
    }

    fn is_data_event(&self) -> bool {
        false
    }

    fn is_error_event(&self) -> bool {
        matches!(self, TcpEvent::AcceptError { .. })
    }
}

/// WebSocket protocol specific events
#[cfg(feature = "websocket")]
#[derive(Debug, Clone)]
pub enum WebSocketEvent {
    HandshakeCompleted {
        session_id: SessionId,
    },
    PingReceived {
        session_id: SessionId,
    },
    PongReceived {
        session_id: SessionId,
    },
    InvalidFrame {
        session_id: SessionId,
        error: String,
    },
}

#[cfg(feature = "websocket")]
impl ProtocolEvent for WebSocketEvent {
    fn into_transport_event(self) -> TransportEvent {
        match self {
            WebSocketEvent::HandshakeCompleted { session_id } => {
                // This should already be handled through ConnectionEstablished
                TransportEvent::ConnectionEstablished {
                    info: ConnectionInfo::default(), // Temporary implementation
                }
            }
            WebSocketEvent::InvalidFrame { session_id, error } => TransportEvent::TransportError {
                error: TransportError::protocol_error("generic", error),
            },
            _ => {
                // Ping/Pong events don't need to be converted to generic events
                TransportEvent::TransportError {
                    error: TransportError::protocol_error(
                        "generic",
                        "Unhandled WebSocket event".to_string(),
                    ),
                }
            }
        }
    }

    fn session_id(&self) -> Option<SessionId> {
        match self {
            WebSocketEvent::HandshakeCompleted { session_id } => Some(*session_id),
            WebSocketEvent::PingReceived { session_id } => Some(*session_id),
            WebSocketEvent::PongReceived { session_id } => Some(*session_id),
            WebSocketEvent::InvalidFrame { session_id, .. } => Some(*session_id),
        }
    }

    fn is_data_event(&self) -> bool {
        matches!(
            self,
            WebSocketEvent::PingReceived { .. } | WebSocketEvent::PongReceived { .. }
        )
    }

    fn is_error_event(&self) -> bool {
        matches!(self, WebSocketEvent::InvalidFrame { .. })
    }
}

/// QUIC protocol specific events
#[cfg(feature = "quic")]
#[derive(Debug, Clone)]
pub enum QuicEvent {
    StreamOpened {
        session_id: SessionId,
        stream_id: u64,
    },
    StreamClosed {
        session_id: SessionId,
        stream_id: u64,
    },
    CertificateVerified {
        session_id: SessionId,
    },
    ConnectionIdRetired {
        session_id: SessionId,
        connection_id: u64,
    },
}

#[cfg(feature = "quic")]
impl ProtocolEvent for QuicEvent {
    fn into_transport_event(self) -> TransportEvent {
        match self {
            QuicEvent::StreamOpened { session_id, .. } => {
                // QUIC stream opening doesn't equal connection establishment, may need special handling
                TransportEvent::ConnectionEstablished {
                    info: ConnectionInfo::default(),
                }
            }
            QuicEvent::StreamClosed { session_id, .. } => TransportEvent::ConnectionClosed {
                reason: CloseReason::Normal,
            },
            _ => {
                // Other QUIC events are not converted for now
                TransportEvent::TransportError {
                    error: TransportError::protocol_error(
                        "generic",
                        "Unhandled QUIC event".to_string(),
                    ),
                }
            }
        }
    }

    fn session_id(&self) -> Option<SessionId> {
        match self {
            QuicEvent::StreamOpened { session_id, .. } => Some(*session_id),
            QuicEvent::StreamClosed { session_id, .. } => Some(*session_id),
            QuicEvent::CertificateVerified { session_id } => Some(*session_id),
            QuicEvent::ConnectionIdRetired { session_id, .. } => Some(*session_id),
        }
    }

    fn is_data_event(&self) -> bool {
        false
    }

    fn is_error_event(&self) -> bool {
        false
    }
}

/// [SIMPLE] User-friendly message structure - unpacked pure data
#[derive(Debug, Clone)]
pub struct Message {
    /// Message source session (None for client, Some for server)
    pub peer: Option<SessionId>,
    /// Decompressed and unpacked raw data
    pub data: Bytes,
    /// Message ID (for debugging and logging)
    pub message_id: u32,
}

impl Message {
    /// Try to convert message data to UTF-8 string
    pub fn as_text(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.data.to_vec())
    }

    /// Convert message data to UTF-8 string (lossy)
    pub fn as_text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }

    /// Get raw byte data
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

/// [SIMPLE] User-friendly request context - unpacked, provides simple response interface
pub struct RequestContext {
    /// Request source session (None for client, Some for server)
    pub peer: Option<SessionId>,
    /// Decompressed and unpacked request data
    pub data: Bytes,
    /// Request ID (for debugging and logging)
    pub request_id: u32,
    /// Business type from packet header
    pub biz_type: u8,
    /// Response callback (handles all protocol details internally)
    responder: Arc<dyn Fn(Bytes) + Send + Sync + 'static>,
    /// Ensure response only once (using Arc shared state)
    responded: Arc<std::sync::atomic::AtomicBool>,
    /// [FLAG] Mark: whether it's the primary instance responsible for checking response (prevents clone instances from triggering warnings)
    is_primary: bool,
}

impl RequestContext {
    /// Create new request context
    pub fn new(
        peer: Option<SessionId>,
        data: impl Into<Bytes>,
        request_id: u32,
        biz_type: u8,
        responder: Arc<dyn Fn(Bytes) + Send + Sync + 'static>,
    ) -> Self {
        Self {
            peer,
            data: data.into(),
            request_id,
            biz_type,
            responder,
            responded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            is_primary: false, // [FLAG] New instances default to non-primary, waiting for event forwarding to set
        }
    }

    /// Try to convert request data to UTF-8 string
    pub fn as_text(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.data.to_vec())
    }

    /// Convert request data to UTF-8 string (lossy)
    pub fn as_text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }

    /// Get raw byte data
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// [SIMPLE] Respond to request using string
    pub fn respond_text(&mut self, response: &str) {
        self.respond_bytes(response.as_bytes());
    }

    /// [SIMPLE] Respond to request using byte data
    pub fn respond_bytes(&mut self, response: &[u8]) {
        if self
            .responded
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
        {
            (self.responder)(Bytes::copy_from_slice(response));
        } else {
            tracing::warn!(
                "[WARN] RequestContext already responded (ID: {})",
                self.request_id
            );
        }
    }

    /// [FLAG] Set as primary instance (responsible for checking response status)
    pub(crate) fn set_primary(&mut self) {
        self.is_primary = true;
    }
}

impl Clone for RequestContext {
    fn clone(&self) -> Self {
        Self {
            peer: self.peer,
            data: self.data.clone(),
            request_id: self.request_id,
            biz_type: self.biz_type,           // [FIX] Share biz_type
            responder: self.responder.clone(), // [FIX] Share response state
            responded: self.responded.clone(), // [FIX] Clone instances are not primary, not responsible for checking response
            is_primary: false, // [FIX] Clone instances are not primary, not responsible for checking response
        }
    }
}

impl std::fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestContext")
            .field("peer", &self.peer)
            .field("data", &format!("{} bytes", self.data.len()))
            .field("request_id", &self.request_id)
            .field("biz_type", &self.biz_type)
            .field(
                "responded",
                &self.responded.load(std::sync::atomic::Ordering::SeqCst),
            )
            .finish()
    }
}

impl Drop for RequestContext {
    fn drop(&mut self) {
        // [FIX] Only primary instances are responsible for checking response status, avoiding false warning from clone instances
        if self.is_primary && !self.responded.load(std::sync::atomic::Ordering::SeqCst) {
            tracing::warn!(
                "[WARN] RequestContext dropped without response (ID: {})",
                self.request_id
            );
        }
    }
}

/// [SIMPLE] User-friendly client events - completely hide Packet complexity
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Connection established
    Connected { info: ConnectionInfo },
    /// Connection disconnected
    Disconnected { reason: CloseReason },

    /// [SIMPLE] Message received (unified context, contains all information)
    MessageReceived(TransportContext),

    /// Message send confirmation
    MessageSent { message_id: u32 },

    /// Transport error
    Error { error: TransportError },
}

impl ClientEvent {
    /// Convert TransportEvent to ClientEvent, hiding session ID
    pub fn from_transport_event(event: TransportEvent) -> Option<Self> {
        match event {
            TransportEvent::ConnectionEstablished { info } => Some(ClientEvent::Connected { info }),
            TransportEvent::ConnectionClosed { reason } => {
                Some(ClientEvent::Disconnected { reason })
            }
            TransportEvent::MessageReceived(packet) => {
                match packet.header.packet_type {
                    crate::packet::PacketType::Request => {
                        // Request packets are specially handled by TransportClient
                        None
                    }
                    _ => {
                        // OneWay and Response packets are handled normally
                        let context = TransportContext::new_oneway(
                            None,
                            packet.header.message_id,
                            packet.header.biz_type,
                            if packet.ext_header.is_empty() {
                                None
                            } else {
                                Some(packet.ext_header.clone())
                            },
                            packet.payload.clone(),
                        );
                        Some(ClientEvent::MessageReceived(context))
                    }
                }
            }
            TransportEvent::MessageSent { packet_id } => Some(ClientEvent::MessageSent {
                message_id: packet_id,
            }),
            TransportEvent::TransportError { error } => Some(ClientEvent::Error { error }),

            _ => None,
        }
    }

    /// Check if it's a connection related event
    pub fn is_connection_event(&self) -> bool {
        matches!(
            self,
            ClientEvent::Connected { .. } | ClientEvent::Disconnected { .. }
        )
    }

    /// Check if it's a data transmission event
    pub fn is_data_event(&self) -> bool {
        matches!(
            self,
            ClientEvent::MessageReceived(..) | ClientEvent::MessageSent { .. }
        )
    }

    /// Check if it's an error event
    pub fn is_error_event(&self) -> bool {
        matches!(self, ClientEvent::Error { .. })
    }
}

/// [TARGET] Unified transport context - used for all received messages
pub struct TransportContext {
    /// Message source session ID (None for client, Some for server)
    pub peer: Option<SessionId>,
    /// System-assigned message ID
    pub message_id: u32,
    /// Business type
    pub biz_type: u8,
    /// Extension header content
    pub ext_header: Option<Vec<u8>>,
    /// Decompressed raw data
    pub data: Bytes,
    /// Reception timestamp
    pub timestamp: Instant,
    /// Message type (internal use)
    kind: TransportContextKind,
}

/// What a respond call actually did.
///
/// `Ok(Written)` is the only value that means "the response bytes reached the
/// socket". Duplicate/late/unknown responds report `AlreadyHandled` instead of
/// a fake success, and real failures are `Err` — the three cases are never
/// conflated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespondOutcome {
    /// The response was written to the transport (write-confirmed).
    Written,
    /// Nothing was written: another responder already claimed this request,
    /// or the request is no longer tracked (late/unknown). Idempotent-skip.
    AlreadyHandled,
}

/// Responder that sends a response and reports the send result. Receives the
/// respond claim (when the request is registry-tracked) so completion
/// ownership can travel with the queued write — the claim resolves the
/// registry state no matter where the send is cancelled or fails. Returns a
/// boxed future so `respond` can spawn it fire-and-forget while
/// `respond_checked` can await the outcome.
type ResponderFn = Arc<
    dyn Fn(
            Bytes,
            Option<crate::transport::request_registry::RespondClaim>,
        ) -> futures::future::BoxFuture<'static, Result<(), crate::error::TransportError>>
        + Send
        + Sync,
>;

/// Message type enumeration
enum TransportContextKind {
    /// One-way message (no response needed)
    OneWay,
    /// Request message (requires response)
    Request {
        responder: ResponderFn,
        responded: Arc<AtomicBool>,
        is_primary: bool, // Mark whether it's the primary instance
        request_registry: Option<Arc<RequestRegistry>>,
        /// Unforgeable registration token: the ONLY key the respond path may
        /// use against the registry (ABA defense via its generation).
        token: Option<crate::transport::request_registry::RequestToken>,
    },
}

impl TransportContext {
    /// Create one-way message context
    pub fn new_oneway(
        peer: Option<SessionId>,
        message_id: u32,
        biz_type: u8,
        ext_header: Option<Vec<u8>>,
        data: impl Into<Bytes>,
    ) -> Self {
        Self {
            peer,
            message_id,
            biz_type,
            ext_header,
            data: data.into(),
            timestamp: Instant::now(),
            kind: TransportContextKind::OneWay,
        }
    }

    pub(crate) fn new_request_with_registry(
        peer: Option<SessionId>,
        message_id: u32,
        biz_type: u8,
        ext_header: Option<Vec<u8>>,
        data: impl Into<Bytes>,
        responder: ResponderFn,
        request_registry: Option<Arc<RequestRegistry>>,
        token: Option<crate::transport::request_registry::RequestToken>,
    ) -> Self {
        Self {
            peer,
            message_id,
            biz_type,
            ext_header,
            data: data.into(),
            timestamp: Instant::now(),
            kind: TransportContextKind::Request {
                responder,
                responded: Arc::new(AtomicBool::new(false)),
                is_primary: false, // Default not primary instance
                request_registry,
                token,
            },
        }
    }

    /// Set as primary instance (responsible for checking response status)
    pub(crate) fn set_primary(&mut self) {
        if let TransportContextKind::Request { is_primary, .. } = &mut self.kind {
            *is_primary = true;
        }
    }

    /// Check if it's a request type
    pub fn is_request(&self) -> bool {
        matches!(self.kind, TransportContextKind::Request { .. })
    }

    /// Convert data to string (lossy conversion)
    pub fn as_text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }

    /// Respond, write-confirmed. `Ok(RespondOutcome::Written)` means the
    /// response bytes reached the socket; duplicate/late/unknown requests
    /// return `Ok(RespondOutcome::AlreadyHandled)` — never a fake `Written`.
    /// One-way messages return a protocol error.
    ///
    /// Consumes the context: a second response is a compile error. The claim
    /// is created in the same poll that wins `begin_respond` and travels with
    /// the queued write, so cancelling this future (timeout/select/abort)
    /// cannot strand registry state, and the registration token's generation
    /// check refuses cross-registration responses (message-id reuse) by
    /// construction.
    pub async fn respond(
        mut self,
        response: impl Into<Bytes>,
    ) -> Result<RespondOutcome, crate::error::TransportError> {
        let response: Bytes = response.into();
        let fut = match &mut self.kind {
            TransportContextKind::Request {
                responder,
                responded,
                request_registry,
                token,
                ..
            } => {
                // Same-poll claim: created and moved into the responder future
                // with no await in between.
                let claim = match (request_registry.as_ref(), token.as_ref()) {
                    (Some(registry), Some(token)) => match registry.begin_respond(token) {
                        MarkResult::Updated => {
                            Some(crate::transport::request_registry::RespondClaim::new(
                                registry.clone(),
                                *token,
                            ))
                        }
                        _ => return Ok(RespondOutcome::AlreadyHandled),
                    },
                    _ => None,
                };
                if responded
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    Some(responder(response, claim))
                } else {
                    None
                }
            }
            TransportContextKind::OneWay => {
                return Err(crate::error::TransportError::protocol_error(
                    "generic",
                    "Cannot respond to one-way message",
                ));
            }
        };
        match fut {
            Some(f) => f.await.map(|()| RespondOutcome::Written),
            None => Ok(RespondOutcome::AlreadyHandled),
        }
    }

    /// Respond without awaiting the outcome. Registry state still resolves
    /// truthfully (the claim rides the queued write); only the caller's
    /// visibility is sacrificed.
    pub fn respond_detached(self, response: impl Into<Bytes>) {
        let response: Bytes = response.into();
        tokio::spawn(async move {
            if let Err(e) = self.respond(response).await {
                tracing::debug!("[RESPOND] detached response send failed: {:?}", e);
            }
        });
    }
}

impl Clone for TransportContext {
    fn clone(&self) -> Self {
        let kind = match &self.kind {
            TransportContextKind::OneWay => TransportContextKind::OneWay,
            TransportContextKind::Request {
                responder,
                responded,
                request_registry,
                token,
                ..
            } => TransportContextKind::Request {
                responder: responder.clone(),
                responded: responded.clone(),
                // Clone instances should never be watchdog owners.
                is_primary: false,
                request_registry: request_registry.clone(),
                token: *token,
            },
        };
        Self {
            peer: self.peer,
            message_id: self.message_id,
            biz_type: self.biz_type,
            ext_header: self.ext_header.clone(),
            data: self.data.clone(),
            timestamp: self.timestamp,
            kind,
        }
    }
}

impl std::fmt::Debug for TransportContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportContext")
            .field("peer", &self.peer)
            .field("message_id", &self.message_id)
            .field("data", &format!("{} bytes", self.data.len()))
            .field("timestamp", &self.timestamp)
            .field("is_request", &self.is_request())
            .finish()
    }
}

impl Drop for TransportContext {
    fn drop(&mut self) {
        if let TransportContextKind::Request {
            responded,
            is_primary,
            ..
        } = &self.kind
        {
            // Drop is now debug-only fallback. Main timeout detection is handled by RequestRegistry.
            if *is_primary && !responded.load(Ordering::SeqCst) {
                tracing::debug!(
                    "[DROP] TransportContext dropped before response (request_id={}, session_id={:?}, biz_type={})",
                    self.message_id,
                    self.peer,
                    self.biz_type
                );
            }
        }
    }
}

/// [TARGET] Unified transport result - return value for all send operations
#[derive(Debug, Clone)]
pub struct TransportResult {
    /// Target session ID (None for client, Some for server)
    pub peer: Option<SessionId>,
    /// System-assigned message ID
    pub message_id: u32,
    /// Send timestamp
    pub timestamp: Instant,
    /// Response data (only for requests, None for sends)
    pub data: Option<Bytes>,
    /// Transport status
    pub status: TransportStatus,
}

/// Transport status enumeration
#[derive(Debug, Clone, PartialEq)]
pub enum TransportStatus {
    /// Send successful
    Sent,
    /// Request timeout
    Timeout,
    /// Connection error
    ConnectionError,
    /// Send successful and response received
    Completed,
}

impl TransportResult {
    /// Create send result  
    pub fn new_sent(peer: Option<SessionId>, message_id: u32) -> Self {
        Self {
            peer,
            message_id,
            timestamp: Instant::now(),
            data: None,
            status: TransportStatus::Sent,
        }
    }

    /// Create request completion result
    pub fn new_completed(peer: Option<SessionId>, message_id: u32, data: impl Into<Bytes>) -> Self {
        Self {
            peer,
            message_id,
            timestamp: Instant::now(),
            data: Some(data.into()),
            status: TransportStatus::Completed,
        }
    }

    /// Create timeout result
    pub fn new_timeout(peer: Option<SessionId>, message_id: u32) -> Self {
        Self {
            peer,
            message_id,
            timestamp: Instant::now(),
            data: None,
            status: TransportStatus::Timeout,
        }
    }

    /// Create connection error result
    pub fn new_connection_error(peer: Option<SessionId>, message_id: u32) -> Self {
        Self {
            peer,
            message_id,
            timestamp: Instant::now(),
            data: None,
            status: TransportStatus::ConnectionError,
        }
    }

    /// Check if send was successful
    pub fn is_sent(&self) -> bool {
        matches!(
            self.status,
            TransportStatus::Sent | TransportStatus::Completed
        )
    }

    /// Check if has response data
    pub fn has_response(&self) -> bool {
        self.data.is_some()
    }
}

#[cfg(test)]
mod respond_tests {
    use super::*;
    use crate::transport::request_registry::RequestToken;

    fn tracked(sid: u64, id: u32) -> (Arc<RequestRegistry>, RequestToken) {
        let registry = Arc::new(RequestRegistry::new());
        registry.open_session(SessionId(sid));
        let token = registry
            .register(
                id,
                Some(SessionId(sid)),
                0,
                std::time::Duration::from_secs(5),
            )
            .expect("registers");
        (registry, token)
    }

    fn responder_with(fut_claim_hold: bool) -> ResponderFn {
        // fut_claim_hold=true: hold the claim inside a never-completing future,
        // modelling a send stuck in the outbound path.
        Arc::new(move |_data, claim| {
            if fut_claim_hold {
                Box::pin(async move {
                    let _hold = claim;
                    std::future::pending::<()>().await;
                    unreachable!()
                })
            } else {
                Box::pin(async move {
                    if let Some(claim) = claim {
                        claim.resolve(true);
                    }
                    Ok(())
                })
            }
        })
    }

    fn tracked_ctx(
        registry: &Arc<RequestRegistry>,
        token: RequestToken,
        responder: ResponderFn,
    ) -> TransportContext {
        TransportContext::new_request_with_registry(
            Some(SessionId(7)),
            1,
            0,
            None,
            b"req".to_vec(),
            responder,
            Some(registry.clone()),
            Some(token),
        )
    }

    #[tokio::test]
    async fn respond_returns_written_from_responder() {
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, responder_with(false));
        assert_eq!(
            ctx.respond(b"resp".to_vec()).await.unwrap(),
            RespondOutcome::Written
        );
    }

    #[tokio::test]
    async fn respond_propagates_send_error() {
        let err: ResponderFn = Arc::new(|_data, _claim| {
            Box::pin(async {
                // _claim drops here: a failed send resolves the registry
                // entry as SendFailed by construction.
                Err(crate::error::TransportError::connection_error(
                    "boom", false,
                ))
            })
        });
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, err);
        assert!(ctx.respond(b"resp".to_vec()).await.is_err());
        assert_eq!(registry.counters_snapshot().response_send_failed_total, 1);
    }

    #[tokio::test]
    async fn respond_on_one_way_is_error() {
        let ctx = TransportContext::new_oneway(Some(SessionId(3)), 44, 0, None, b"data".to_vec());
        assert!(ctx.respond(b"resp".to_vec()).await.is_err());
    }

    /// Cancelling a respond mid-send must not strand the registry in
    /// Responding: the claim travels with the responder future, so dropping
    /// the caller's future resolves the entry as a send failure.
    #[tokio::test]
    async fn cancelled_respond_resolves_the_claim() {
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, responder_with(true));

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            ctx.respond(b"resp".to_vec()),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "send must still be in flight at timeout"
        );
        // The timeout dropped the respond future -> claim dropped -> resolved.
        assert_eq!(
            registry.get_state(
                Some(SessionId(7)),
                1,
                crate::transport::request_registry::RequestDirection::Inbound
            ),
            None,
            "cancelled respond must not leave a Responding entry"
        );
        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.response_send_failed_total, 1);
        assert_eq!(registry.active_len(), 0);
    }

    /// A context whose token belongs to an earlier registration of a reused
    /// message id reports AlreadyHandled — it can never claim the new one.
    #[tokio::test]
    async fn stale_context_reports_already_handled_after_id_reuse() {
        let (registry, old_token) = tracked(7, 1);
        let stale = tracked_ctx(&registry, old_token, responder_with(false));

        // First life ends and the id is reused by a new registration.
        assert_eq!(registry.begin_respond(&old_token), MarkResult::Updated);
        assert_eq!(
            registry.finish_respond(&old_token, true),
            MarkResult::Updated
        );
        let new_token = registry
            .register(1, Some(SessionId(7)), 0, std::time::Duration::from_secs(5))
            .expect("re-registers");

        assert_eq!(
            stale.respond(b"late".to_vec()).await.unwrap(),
            RespondOutcome::AlreadyHandled
        );
        // The new registration is untouched and answerable.
        assert_eq!(registry.begin_respond(&new_token), MarkResult::Updated);
        assert_eq!(
            registry.finish_respond(&new_token, true),
            MarkResult::Updated
        );
    }
}

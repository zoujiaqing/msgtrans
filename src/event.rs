use crate::command::ConnectionInfo;
use crate::error::TransportError;
use crate::packet::Packet;
use crate::{CloseReason, PacketId, SessionId};
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
            TransportEvent::RequestReceived(..) => None,
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
#[derive(Debug, Clone)]
pub enum TcpEvent {
    ListenerBound { addr: SocketAddr },
    AcceptError { error: String },
    ConnectionTimeout { session_id: SessionId },
}

impl ProtocolEvent for TcpEvent {
    fn into_transport_event(self) -> TransportEvent {
        match self {
            TcpEvent::ListenerBound { addr } => TransportEvent::ServerStarted { address: addr },
            TcpEvent::AcceptError { error } => TransportEvent::TransportError {
                error: TransportError::connection_error(
                    format!(
                        "IO error: {:?}",
                        std::io::Error::new(std::io::ErrorKind::Other, error)
                    ),
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
    pub data: Vec<u8>,
    /// Message ID (for debugging and logging)
    pub message_id: u32,
}

impl Message {
    /// Try to convert message data to UTF-8 string
    pub fn as_text(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.data.clone())
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
    pub data: Vec<u8>,
    /// Request ID (for debugging and logging)
    pub request_id: u32,
    /// Business type from packet header
    pub biz_type: u8,
    /// Response callback (handles all protocol details internally)
    responder: Arc<dyn Fn(Vec<u8>) + Send + Sync + 'static>,
    /// Ensure response only once (using Arc shared state)
    responded: Arc<std::sync::atomic::AtomicBool>,
    /// [FLAG] Mark: whether it's the primary instance responsible for checking response (prevents clone instances from triggering warnings)
    is_primary: bool,
}

impl RequestContext {
    /// Create new request context
    pub fn new(
        peer: Option<SessionId>,
        data: Vec<u8>,
        request_id: u32,
        biz_type: u8,
        responder: Arc<dyn Fn(Vec<u8>) + Send + Sync + 'static>,
    ) -> Self {
        Self {
            peer,
            data,
            request_id,
            biz_type,
            responder,
            responded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            is_primary: false, // [FLAG] New instances default to non-primary, waiting for event forwarding to set
        }
    }

    /// Try to convert request data to UTF-8 string
    pub fn as_text(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.data.clone())
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
            (self.responder)(response.to_vec());
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

/// [SIMPLE] User-friendly server events - completely hide Packet complexity  
#[derive(Debug, Clone)]
pub enum ServerEvent {
    /// New connection established
    ConnectionEstablished {
        session_id: SessionId,
        info: ConnectionInfo,
    },
    /// Connection closed
    ConnectionClosed {
        session_id: SessionId,
        reason: CloseReason,
    },

    /// [SIMPLE] Message received (unified context, contains all information)
    MessageReceived {
        session_id: SessionId,
        context: TransportContext,
    },

    /// Message send confirmation
    MessageSent {
        session_id: SessionId,
        message_id: u32,
    },

    /// Transport error
    TransportError {
        session_id: Option<SessionId>,
        error: TransportError,
    },

    /// Server lifecycle events
    ServerStarted {
        address: std::net::SocketAddr,
    },
    ServerStopped,
}

impl ServerEvent {
    pub fn from_transport_event(event: TransportEvent) -> Option<Self> {
        match event {
            TransportEvent::ConnectionEstablished { info } => None,
            TransportEvent::ConnectionClosed { reason } => None,
            TransportEvent::MessageReceived(packet) => None,
            TransportEvent::MessageSent { packet_id } => None,
            TransportEvent::TransportError { error } => None,
            TransportEvent::ServerStarted { address } => {
                Some(ServerEvent::ServerStarted { address })
            }
            TransportEvent::ServerStopped => Some(ServerEvent::ServerStopped),
            TransportEvent::RequestReceived(ctx) => None,
            _ => None,
        }
    }

    /// New addition: Convert TransportEvent to ServerEvent with session_id
    pub fn from_transport_event_with_session(
        event: TransportEvent,
        session_id: SessionId,
    ) -> Option<Self> {
        match event {
            TransportEvent::ConnectionEstablished { info } => {
                Some(ServerEvent::ConnectionEstablished { session_id, info })
            }
            TransportEvent::ConnectionClosed { reason } => {
                Some(ServerEvent::ConnectionClosed { session_id, reason })
            }
            TransportEvent::MessageReceived(packet) => {
                // Convert to unified TransportContext
                let context = TransportContext::new_oneway(
                    Some(session_id),
                    packet.header.message_id,
                    packet.header.biz_type,
                    if packet.ext_header.is_empty() {
                        None
                    } else {
                        Some(packet.ext_header.clone())
                    },
                    packet.payload.clone(),
                );
                Some(ServerEvent::MessageReceived {
                    session_id,
                    context,
                })
            }
            TransportEvent::MessageSent { packet_id } => Some(ServerEvent::MessageSent {
                session_id,
                message_id: packet_id,
            }),
            TransportEvent::TransportError { error } => Some(ServerEvent::TransportError {
                session_id: Some(session_id),
                error,
            }),
            TransportEvent::RequestReceived(ctx) => {
                // 🔧 Convert RequestContext to TransportContext
                let transport_ctx = TransportContext::new_request(
                    Some(session_id),
                    ctx.request_id,
                    ctx.biz_type, // Use biz_type from RequestContext
                    None, // Default no extension header, needs to be passed from RequestContext
                    ctx.data.clone(),
                    ctx.responder.clone(),
                );
                Some(ServerEvent::MessageReceived {
                    session_id,
                    context: transport_ctx,
                })
            }
            TransportEvent::ServerStarted { address } => {
                Some(ServerEvent::ServerStarted { address })
            }
            TransportEvent::ServerStopped => Some(ServerEvent::ServerStopped),
            _ => None,
        }
    }
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
#[derive(Clone)]
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
    pub data: Vec<u8>,
    /// Reception timestamp
    pub timestamp: Instant,
    /// Message type (internal use)
    kind: TransportContextKind,
}

/// Message type enumeration
#[derive(Clone)]
enum TransportContextKind {
    /// One-way message (no response needed)
    OneWay,
    /// Request message (requires response)
    Request {
        responder: Arc<dyn Fn(Vec<u8>) + Send + Sync + 'static>,
        responded: Arc<AtomicBool>,
        is_primary: bool, // Mark whether it's the primary instance
    },
}

impl TransportContext {
    /// Create one-way message context
    pub fn new_oneway(
        peer: Option<SessionId>,
        message_id: u32,
        biz_type: u8,
        ext_header: Option<Vec<u8>>,
        data: Vec<u8>,
    ) -> Self {
        Self {
            peer,
            message_id,
            biz_type,
            ext_header,
            data,
            timestamp: Instant::now(),
            kind: TransportContextKind::OneWay,
        }
    }

    /// Create request message context
    pub fn new_request(
        peer: Option<SessionId>,
        message_id: u32,
        biz_type: u8,
        ext_header: Option<Vec<u8>>,
        data: Vec<u8>,
        responder: Arc<dyn Fn(Vec<u8>) + Send + Sync + 'static>,
    ) -> Self {
        Self {
            peer,
            message_id,
            biz_type,
            ext_header,
            data,
            timestamp: Instant::now(),
            kind: TransportContextKind::Request {
                responder,
                responded: Arc::new(AtomicBool::new(false)),
                is_primary: false, // Default not primary instance
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

    /// Respond to request (only available for request type)
    pub fn respond(mut self, response: Vec<u8>) {
        match &mut self.kind {
            TransportContextKind::Request {
                responder,
                responded,
                ..
            } => {
                if responded
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    responder(response);
                } else {
                    tracing::warn!(
                        "[WARN] TransportContext already responded (ID: {})",
                        self.message_id
                    );
                }
            }
            TransportContextKind::OneWay => {
                tracing::warn!(
                    "[WARN] Cannot respond to one-way message (ID: {})",
                    self.message_id
                );
            }
        }
    }

    /// Convenience method: respond with byte data
    pub fn respond_bytes(self, response: &[u8]) {
        self.respond(response.to_vec());
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
            // Only primary instance checks response status
            if *is_primary && !responded.load(Ordering::SeqCst) {
                // [PERF] Fix: delayed check, give application layer some time to handle event
                // Clone data needed for checking
                let message_id = self.message_id;
                let responded_clone = responded.clone();

                // Perform delayed check in separate task
                tokio::spawn(async move {
                    // Give application layer 10ms to handle event
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                    // Check again if responded
                    if !responded_clone.load(Ordering::SeqCst) {
                        tracing::warn!(
                            "[WARN] TransportContext dropped without response (ID: {})",
                            message_id
                        );
                    }
                });
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
    pub data: Option<Vec<u8>>,
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
    pub fn new_completed(peer: Option<SessionId>, message_id: u32, data: Vec<u8>) -> Self {
        Self {
            peer,
            message_id,
            timestamp: Instant::now(),
            data: Some(data),
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

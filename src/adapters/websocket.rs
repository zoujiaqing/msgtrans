use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    accept_hdr_async_with_config, connect_async_tls_with_config,
    tungstenite::{
        error,
        handshake::server::{ErrorResponse, Request, Response},
        protocol::{Message, WebSocketConfig},
        Error as TungsteniteError,
    },
    Connector, MaybeTlsStream, WebSocketStream,
};

use crate::{
    command::ConnectionState, connection::Connection, error::TransportError, event::TransportEvent,
    packet::Packet, ConnectionInfo, SessionId,
};

/// WebSocket message processing result
#[derive(Debug)]
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

/// Ping/pong + idle enforcement parameters, extracted from either config side.
#[derive(Debug, Clone, Copy, Default)]
struct WsKeepalive {
    ping_interval: Option<std::time::Duration>,
    pong_timeout: std::time::Duration,
    idle_timeout: Option<std::time::Duration>,
}

fn ws_keepalive_of<C: std::any::Any>(config: &C) -> WsKeepalive {
    let any = config as &dyn std::any::Any;
    if let Some(c) = any.downcast_ref::<crate::protocol::WebSocketServerConfig>() {
        return WsKeepalive {
            ping_interval: c.ping_interval,
            pong_timeout: c.pong_timeout,
            idle_timeout: c.idle_timeout,
        };
    }
    if let Some(c) = any.downcast_ref::<crate::protocol::WebSocketClientConfig>() {
        return WsKeepalive {
            ping_interval: c.ping_interval,
            pong_timeout: c.pong_timeout,
            idle_timeout: None,
        };
    }
    WsKeepalive::default()
}

/// Frame/message caps for the tungstenite protocol layer, DERIVED from this
/// connection's [`crate::packet::DecodeLimits`].
///
/// tungstenite used to carry its own `max_frame_size`/`max_message_size` on the
/// WebSocket config — a second source of truth that disagreed with the one
/// msgtrans enforces. Both directions were wrong: the default msgtrans frame
/// (16 B header + 64 KiB ext + 16 MiB payload) exceeded tungstenite's 16 MiB
/// frame cap, so a legal boundary frame was rejected before msgtrans ever saw
/// it; and a caller who set a small `ServerLimits` still let tungstenite
/// buffer up to 64 MiB, so the resource limit was not the real one.
///
/// msgtrans puts exactly one packet in one binary WebSocket message, so the
/// message cap and the frame cap are the same number: our maximum frame.
fn ws_protocol_config(limits: &crate::packet::DecodeLimits) -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(limits.max_frame_size))
        .max_frame_size(Some(limits.max_frame_size))
}

/// Build the TLS connector for wss:// per the configured [`ClientTls`]
/// behavior. ws:// connections ignore it.
fn build_tls_connector(
    tls: &crate::protocol::client_config::ClientTls,
) -> Result<Connector, WebSocketError> {
    use crate::protocol::client_config::ClientTls;
    // Explicit crypto provider: never rely on the process-level default,
    // which panics when a downstream links more than one rustls provider.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = || {
        rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("ring provider supports default TLS versions")
    };
    let crypto = match tls {
        ClientTls::SystemRoots => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
        ClientTls::CustomCa(pem) => {
            let mut roots = rustls::RootCertStore::empty();
            let certs = rustls_pemfile::certs(&mut std::io::Cursor::new(pem.as_bytes()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| WebSocketError::Config(format!("Invalid CA PEM: {e}")))?;
            if certs.is_empty() {
                return Err(WebSocketError::Config(
                    "CA PEM contains no certificates".to_string(),
                ));
            }
            for cert in certs {
                roots
                    .add(cert)
                    .map_err(|e| WebSocketError::Config(format!("Invalid CA certificate: {e}")))?;
            }
            builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
        ClientTls::Insecure => {
            tracing::warn!(
                "[SECURITY] WebSocket client skipping certificate verification; this is insecure"
            );
            builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(SkipWsServerVerification::new()))
                .with_no_client_auth()
        }
    };
    Ok(Connector::Rustls(Arc::new(crypto)))
}

/// Certificate verifier that accepts anything — [`ClientTls::Insecure`] only.
#[derive(Debug)]
struct SkipWsServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl SkipWsServerVerification {
    fn new() -> Self {
        Self(Arc::new(rustls::crypto::ring::default_provider()))
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipWsServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
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
    /// Connection liveness + session id, shared with the event loop.
    state: crate::adapters::core::ConnState,
    /// Configuration (retained to keep the generic `C` and for diagnostics).
    #[allow(dead_code)]
    config: C,
    /// Statistics information (diagnostics; not on the hot path).
    #[allow(dead_code)]
    /// Connection information
    connection_info: ConnectionInfo,
    /// Send queue
    send_queue: mpsc::Sender<crate::adapters::outbound::Outbound>,
    /// Event sender
    event_pipe_rx: Option<crate::adapters::events::EventPipeRx>,
    /// Shutdown signal sender
    shutdown_sender: mpsc::UnboundedSender<()>,
    /// Event loop handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Frame decode policy (0=Lenient, 1=Strict), shared with the event loop.
    frame_policy: Arc<std::sync::atomic::AtomicU8>,
}

impl<C> WebSocketAdapter<C> {
    /// Create adapter with WebSocket stream
    pub async fn new_with_stream(
        config: C,
        stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self, WebSocketError>
    where
        C: std::any::Any,
    {
        let mut connection_info = ConnectionInfo::default();
        connection_info.protocol = "websocket".to_string();
        connection_info.state = ConnectionState::Connected;
        connection_info.established_at = std::time::SystemTime::now();
        // Real addresses off the underlying TCP socket. These used to be left
        // at the 0.0.0.0:0 default, so every handler saw a fake peer address.
        {
            let tcp = match stream.get_ref() {
                MaybeTlsStream::Plain(tcp) => Some(tcp),
                #[cfg(feature = "websocket")]
                MaybeTlsStream::Rustls(tls) => Some(tls.get_ref().0),
                _ => None,
            };
            if let Some(tcp) = tcp {
                if let Ok(peer) = tcp.peer_addr() {
                    connection_info.peer_addr = peer;
                }
                if let Ok(local) = tcp.local_addr() {
                    connection_info.local_addr = local;
                }
            }
        }

        // The stream is already established when this adapter is created.
        let state =
            crate::adapters::core::ConnState::new(crate::adapters::core::ConnStatus::Connected);
        let frame_policy = Arc::new(std::sync::atomic::AtomicU8::new(
            crate::packet::FramePolicy::default() as u8,
        ));

        // Create communication channels
        let (send_queue_tx, send_queue_rx) = mpsc::channel(limits.outbound_capacity);
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();
        // Bounded event backbone: the loop task owns the sender half; when the
        // loop ends the pipe drops and the consumer sees end-of-data.
        let (event_pipe, event_pipe_rx) = crate::adapters::events::event_pipe(limits.pipe_capacity);

        // Keepalive/idle enforcement parameters from the config (previously
        // declared but never enforced).
        let keepalive = ws_keepalive_of(&config);

        // Start event loop
        let event_loop_handle = Self::start_event_loop(
            stream,
            state.clone(),
            send_queue_rx,
            shutdown_rx,
            event_pipe,
            frame_policy.clone(),
            keepalive,
            limits.write_deadline,
            limits.decode_limits(),
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

    /// Start event loop based on tokio::select!
    #[allow(clippy::too_many_arguments)]
    async fn start_event_loop(
        mut stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
        state: crate::adapters::core::ConnState,
        mut send_queue: mpsc::Receiver<crate::adapters::outbound::Outbound>,
        mut shutdown_signal: mpsc::UnboundedReceiver<()>,
        event_pipe: crate::adapters::events::EventPipe,
        frame_policy: Arc<std::sync::atomic::AtomicU8>,
        keepalive: WsKeepalive,
        write_deadline: std::time::Duration,
        decode_limits: crate::packet::DecodeLimits,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let current_session_id = state.session_id();
            tracing::debug!(
                "[START] WebSocket event loop started (session: {})",
                current_session_id
            );

            // Keepalive/idle state: any traffic in either direction counts as
            // activity; an unanswered ping past pong_timeout is a dead peer.
            let mut last_activity = tokio::time::Instant::now();
            let mut next_ping =
                tokio::time::Instant::now() + keepalive.ping_interval.unwrap_or_default();
            let mut ping_sent_at: Option<tokio::time::Instant> = None;

            loop {
                // Get current session ID
                let current_session_id = state.session_id();

                tokio::select! {
                    // [IDLE] No traffic in either direction for the configured
                    // duration closes the connection.
                    _ = tokio::time::sleep_until(last_activity + keepalive.idle_timeout.unwrap_or_default()), if keepalive.idle_timeout.is_some() => {
                        tracing::info!("[IDLE] WebSocket connection idle for {:?}, closing (session: {})", keepalive.idle_timeout.unwrap(), current_session_id);
                        event_pipe.close(crate::error::CloseReason::Timeout);
                        state.set_status(crate::adapters::core::ConnStatus::Closed);
                        break;
                    }

                    // [PING] Two separate deadlines share this arm: while a
                    // ping is outstanding the wake-up is its pong deadline
                    // (dead-peer detection); otherwise it is the next
                    // interval-scheduled ping. A healthy connection therefore
                    // pings exactly once per ping_interval — the pong_timeout
                    // never shortens the cadence.
                    _ = tokio::time::sleep_until(match ping_sent_at {
                        Some(sent_at) => sent_at + keepalive.pong_timeout,
                        None => next_ping,
                    }), if keepalive.ping_interval.is_some() => {
                        if ping_sent_at.is_some() {
                            // Pong deadline elapsed without a pong.
                            tracing::info!("[PING] WebSocket pong timeout ({:?}), closing (session: {})", keepalive.pong_timeout, current_session_id);
                            event_pipe.close(crate::error::CloseReason::Timeout);
                            state.set_status(crate::adapters::core::ConnStatus::Closed);
                            break;
                        }
                        if let Err(e) = stream.send(Message::Ping(Bytes::new())).await {
                            tracing::debug!("[PING] WebSocket ping send failed: {:?} (session: {})", e, current_session_id);
                            event_pipe.close(crate::error::CloseReason::Error(format!("{:?}", e)));
                            state.set_status(crate::adapters::core::ConnStatus::Closed);
                            break;
                        }
                        ping_sent_at = Some(tokio::time::Instant::now());
                        next_ping = tokio::time::Instant::now() + keepalive.ping_interval.unwrap();
                    }

                    // [RECV] Handle incoming data
                    read_result = stream.next() => {
                        match read_result {
                            Some(Ok(message)) => {
                                last_activity = tokio::time::Instant::now();
                                if matches!(message, Message::Pong(_)) {
                                    ping_sent_at = None;
                                }
                                let policy = crate::packet::FramePolicy::from_u8(
                                    frame_policy.load(std::sync::atomic::Ordering::Relaxed),
                                );
                                match Self::process_websocket_message(message, policy, &decode_limits) {
                                    MessageProcessResult::Packet(packet) => {
                                        tracing::debug!("[RECV] WebSocket received packet: {} bytes (session: {})", packet.payload.len(), current_session_id);

                                        // Send receive event
                                        // Data plane: backpressure, no loss.
                                        if !event_pipe
                                            .deliver(TransportEvent::MessageReceived(packet))
                                            .await
                                        {
                                            break;
                                        }
                                    }
                                    MessageProcessResult::Heartbeat => {
                                        // Heartbeat message, continue loop
                                        continue;
                                    }
                                    MessageProcessResult::PeerClosed => {
                                        // Peer closed normally: notify upper layer application that connection is closed for resource cleanup
                                        event_pipe.close(crate::error::CloseReason::Normal);
                                        state.set_status(crate::adapters::core::ConnStatus::Closed);
                                        break;
                                    }
                                    MessageProcessResult::Error(e) => {
                                        tracing::error!("[ERROR] WebSocket message processing error: {:?} (session: {})", e, current_session_id);
                                        // Message processing error: notify upper layer application of connection error for resource cleanup
                                        event_pipe.close(crate::error::CloseReason::Error(format!("{:?}", e)));
                                        state.set_status(crate::adapters::core::ConnStatus::Closed);
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
                                event_pipe.close(reason);
                                state.set_status(crate::adapters::core::ConnStatus::Closed);
                                break;
                            }
                            None => {
                                tracing::debug!("[CLOSE] Peer actively closed WebSocket connection (session: {})", current_session_id);
                                // Peer actively closed: notify upper layer application that connection is closed for resource cleanup
                                event_pipe.close(crate::error::CloseReason::Normal);
                                state.set_status(crate::adapters::core::ConnStatus::Closed);
                                break;
                            }
                        }
                    }

                    // [SEND] Handle outgoing data - zero-copy optimization
                    item = send_queue.recv() => {
                        if let Some(item) = item {
                            let (packet, completion) = (item.packet, item.completion);
                            // tungstenite takes Bytes directly, so the packet's own
                            // Bytes buffer is handed over without another copy.
                            let encoded = match packet.try_encode() {
                                Ok(bytes) => bytes,
                                Err(e) => {
                                    if let Some(completion) = completion {
                                        completion.complete(Err(crate::error::TransportError::protocol_error("websocket", format!("encode failed: {e}"))));
                                    }
                                    continue;
                                }
                            };
                            let message = Message::Binary(encoded);

                            // Deadline: a peer that stops draining kills the
                            // connection instead of blocking queued senders.
                            let write = tokio::time::timeout(
                                write_deadline,
                                stream.send(message),
                            ).await;
                            match write {
                                Ok(Ok(_)) => {
                                    last_activity = tokio::time::Instant::now();
                                    tracing::debug!("[SEND] WebSocket send successful: {} bytes (session: {})", packet.payload.len(), current_session_id);
                                    if let Some(completion) = completion {
                                        completion.complete(Ok(()));
                                    }
                                    // Send send event
                                    event_pipe.diagnostic(TransportEvent::MessageSent { packet_id: packet.header.message_id });
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("[ERROR] WebSocket send error: {:?} (session: {})", e, current_session_id);
                                    if let Some(completion) = completion {
                                        completion.complete(Err(crate::error::TransportError::connection_error(
                                            "WebSocket write failed",
                                            false,
                                        )));
                                    }
                                    // Send error: notify upper layer application of connection error for resource cleanup
                                    event_pipe.close(crate::error::CloseReason::Error(format!("{:?}", e)));
                                    state.set_status(crate::adapters::core::ConnStatus::Closed);
                                    break;
                                }
                                Err(_) => {
                                    tracing::error!("[ERROR] WebSocket write deadline exceeded (session: {})", current_session_id);
                                    if let Some(completion) = completion {
                                        completion.complete(Err(crate::error::TransportError::connection_error(
                                            "WebSocket write deadline exceeded",
                                            false,
                                        )));
                                    }
                                    event_pipe.close(crate::error::CloseReason::Error(
                                        "WebSocket write deadline exceeded".to_string(),
                                    ));
                                    state.set_status(crate::adapters::core::ConnStatus::Closed);
                                    break;
                                }
                            }
                        }
                    }

                    // [STOP] Handle shutdown signal
                    _ = shutdown_signal.recv() => {
                        tracing::info!("[STOP] Received shutdown signal, stopping WebSocket event loop (session: {})", current_session_id);
                        // Locally initiated close is Normal, not an abnormal end.
                        event_pipe.close(crate::error::CloseReason::Normal);
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
    fn process_websocket_message(
        message: Message,
        frame_policy: crate::packet::FramePolicy,
        decode_limits: &crate::packet::DecodeLimits,
    ) -> MessageProcessResult {
        let strict = frame_policy == crate::packet::FramePolicy::Strict;
        match message {
            Message::Binary(data) => {
                // Pre-check minimum length.
                if data.len() < 16 {
                    if strict {
                        return MessageProcessResult::Error(WebSocketError::InvalidMessageType);
                    }
                    let packet = Packet::one_way(0, data.clone());
                    return MessageProcessResult::Packet(packet);
                }

                // Zero-copy: `data` is an owned Bytes (tungstenite), so
                // decode_exact_from slices the body out without a payload copy.
                match Packet::decode_exact_from(&data, decode_limits) {
                    Ok(packet) => {
                        tracing::debug!(
                            "[RECV] WebSocket packet parsing successful: {} bytes",
                            packet.payload.len()
                        );
                        MessageProcessResult::Packet(packet)
                    }
                    Err(e) => {
                        if strict {
                            tracing::debug!(
                                "[RECV] WebSocket packet parse failed under strict policy: {:?}",
                                e
                            );
                            return MessageProcessResult::Error(WebSocketError::InvalidMessageType);
                        }
                        tracing::debug!("[RECV] WebSocket packet parsing failed: {:?}, creating basic data packet", e);
                        let packet = Packet::one_way(0, data.clone());
                        MessageProcessResult::Packet(packet)
                    }
                }
            }
            Message::Text(text) => {
                // msgtrans is a binary protocol: a text frame is a protocol
                // violation under Strict (matches the TypeScript SDK default).
                // Lenient keeps the 1.x debugging behavior of wrapping the
                // text bytes in a raw one-way packet.
                if strict {
                    tracing::debug!(
                        "[RECV] WebSocket text frame rejected under strict policy ({} bytes)",
                        text.len()
                    );
                    return MessageProcessResult::Error(WebSocketError::InvalidMessageType);
                }
                tracing::debug!(
                    "[RECV] WebSocket received text message: {} bytes",
                    text.len()
                );
                let packet = Packet::one_way(0, Bytes::copy_from_slice(text.as_bytes()));
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
impl<C: Send + Sync + 'static> Connection for WebSocketAdapter<C> {
    fn writer(&self) -> std::sync::Arc<dyn crate::connection::ConnectionWriter> {
        std::sync::Arc::new(crate::adapters::outbound::QueueWriter::new(
            self.send_queue.clone(),
            "websocket_outbound_queue",
            "WebSocket connection closed",
        ))
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        let current_session_id = self.state.session_id();
        tracing::debug!(
            "[CLOSE] Close WebSocket connection (session: {})",
            current_session_id
        );

        let _ = self.shutdown_sender.send(());

        if let Some(handle) = self.event_loop_handle.take() {
            let _ = handle.await;
        }

        self.state
            .set_status(crate::adapters::core::ConnStatus::Closed);
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
        // Refresh the live session id: `set_session_id` keeps the stored copy
        // in sync, but reading from `state` is the single source of truth.
        let mut info = self.connection_info.clone();
        info.session_id = self.state.session_id();
        info
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

pub(crate) struct WebSocketServerBuilder<C> {
    config: Option<C>,
    limits: crate::transport::limits::ConnectionLimits,
}

impl<C> WebSocketServerBuilder<C> {
    pub(crate) fn new() -> Self {
        Self {
            config: None,
            limits: crate::transport::limits::ConnectionLimits::default(),
        }
    }

    pub(crate) fn limits(mut self, limits: crate::transport::limits::ConnectionLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) fn config(mut self, config: C) -> Self {
        self.config = Some(config);
        self
    }

    /// Override the bind address on the concrete config. Previously a no-op:
    /// the public factory passed the requested address into it and the server
    /// silently bound the config default instead.
    pub(crate) fn bind_address(mut self, addr: std::net::SocketAddr) -> Self
    where
        C: std::any::Any,
    {
        if let Some(cfg) = self.config.as_mut().and_then(|c| {
            (c as &mut dyn std::any::Any).downcast_mut::<crate::protocol::WebSocketServerConfig>()
        }) {
            cfg.bind_address = addr;
        }
        self
    }

    pub(crate) async fn build(self) -> Result<WebSocketServer<C>, WebSocketError> {
        let config = self
            .config
            .ok_or_else(|| WebSocketError::Config("Missing WebSocket server config".to_string()))?;
        Ok(WebSocketServer {
            config,
            limits: self.limits,
            local_addr: None,
            incoming: None,
            pump: None,
        })
    }
}

/// Upper bound on concurrently pending WebSocket upgrades. A flood of peers
/// that open sockets and never speak cannot exhaust tasks or memory.
const MAX_PENDING_WS_HANDSHAKES: usize = 256;

/// A WebSocket upgrade that has not completed within this window is abandoned.
///
/// 🔴 生产事故（2026-09，Weey :9080）：此前升级握手在 `accept()` 里**同步等待**
/// 且没有超时。一个连上来后一个字节都不发的对端（公网扫描器就是这种行为）会让
/// `accept_hdr_async` 永远挂着，后面所有连接都排在它后面：内核 accept 队列灌满
/// （Recv-Q 129/128），新客户端 TCP 能连上但握手永远超时，而进程本身一条异常日志
/// 都没有。TCP+TLS 路径早已改成「后台泵 + 并发握手 + 超时」，这里对齐同一模式。
const WS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A completed upgrade handed from the accept pump to `accept()`.
type AcceptedWebSocket = (
    WebSocketStream<MaybeTlsStream<TcpStream>>,
    std::net::SocketAddr,
);

pub(crate) struct WebSocketServer<C> {
    config: C,
    limits: crate::transport::limits::ConnectionLimits,
    /// Bound address, captured when the listener is created so it survives the
    /// listener being moved into the background accept pump.
    local_addr: Option<std::net::SocketAddr>,
    /// Completed upgrades. `None` until the first `accept()` binds the port.
    incoming: Option<mpsc::Receiver<AcceptedWebSocket>>,
    /// Handle to the background pump, aborted on shutdown/drop.
    pump: Option<tokio::task::JoinHandle<()>>,
}

impl<C: 'static> WebSocketServer<C> {
    fn server_config(&self) -> crate::protocol::WebSocketServerConfig {
        (&self.config as &dyn std::any::Any)
            .downcast_ref::<crate::protocol::WebSocketServerConfig>()
            .cloned()
            .unwrap_or_default()
    }

    /// Bind the port and start the accept pump. The pump owns the listener:
    /// it accepts raw sockets and runs each upgrade handshake on its own task
    /// under `WS_HANDSHAKE_TIMEOUT`, so a slow or silent peer can never stall
    /// the connections behind it.
    async fn start_pump(&mut self) -> Result<(), WebSocketError> {
        let ws_cfg = self.server_config();
        let listener = TcpListener::bind(ws_cfg.bind_address).await?;
        let local_addr = listener.local_addr()?;
        tracing::debug!("[START] WebSocket server listening on: {}", local_addr);

        let accept_limits = self.limits;
        let expected_path = ws_cfg.path;
        let supported: Vec<String> = ws_cfg.subprotocols;
        let (tx, rx) = mpsc::channel::<AcceptedWebSocket>(MAX_PENDING_WS_HANDSHAKES);
        let permits = Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_WS_HANDSHAKES));

        let pump = tokio::spawn(async move {
            loop {
                let (tcp_stream, addr) = match listener.accept().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("[WS] accept failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        continue;
                    }
                };
                tracing::debug!("[ACCEPT] WebSocket server accepted connection: {}", addr);
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    tracing::warn!("[WS] too many pending handshakes, dropping {addr}");
                    continue;
                };
                let tx = tx.clone();
                let expected_path = expected_path.clone();
                let supported = supported.clone();
                tokio::spawn(async move {
                    let handshake =
                        ws_upgrade(tcp_stream, expected_path, supported, &accept_limits);
                    match tokio::time::timeout(WS_HANDSHAKE_TIMEOUT, handshake).await {
                        Ok(Ok(ws_stream)) => {
                            // Shed completed upgrades when the bounded accept queue is full.
                            if tx.try_send((ws_stream, addr)).is_err() {
                                tracing::warn!("[WS] accept queue full, dropping {addr}");
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::debug!("[WS] upgrade failed from {addr}: {e}")
                        }
                        Err(_) => tracing::debug!("[WS] upgrade timed out from {addr}"),
                    }
                    drop(permit);
                });
            }
        });

        self.local_addr = Some(local_addr);
        self.incoming = Some(rx);
        self.pump = Some(pump);
        Ok(())
    }

    pub(crate) async fn accept(&mut self) -> Result<WebSocketAdapter<C>, WebSocketError>
    where
        C: Clone + crate::protocol::ProtocolConfig,
    {
        if self.incoming.is_none() {
            self.start_pump().await?;
        }
        let rx = self
            .incoming
            .as_mut()
            .ok_or_else(|| WebSocketError::Config("No listener available".to_string()))?;
        // The pump already accepted the socket and finished the upgrade, so
        // nothing here can be stalled by a slow peer.
        let (ws_stream, addr) = rx
            .recv()
            .await
            .ok_or_else(|| WebSocketError::Config("WebSocket server is shut down".to_string()))?;
        tracing::debug!("[ACCEPT] WebSocket upgrade completed: {}", addr);

        WebSocketAdapter::new_with_stream(self.config.clone(), ws_stream, self.limits).await
    }

    pub(crate) fn local_addr(&self) -> Result<std::net::SocketAddr, WebSocketError> {
        self.local_addr
            .ok_or_else(|| WebSocketError::Config("Server not bound".to_string()))
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), WebSocketError> {
        // Abort the pump to release the TCP port without waiting for task drop.
        self.incoming.take();
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
        self.local_addr = None;
        Ok(())
    }
}

impl<C> Drop for WebSocketServer<C> {
    fn drop(&mut self) {
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
    }
}

/// Run the HTTP → WebSocket upgrade for one accepted socket with the configured
/// contract enforced:
/// - request path must match config.path (404 otherwise)
/// - a client-offered subprotocol is negotiated and echoed when it matches;
///   peers that offer none are accepted (not required)
/// - tungstenite enforces THIS CONNECTION's frame caps, derived from
///   ConnectionLimits (not a second set on the ws config)
async fn ws_upgrade(
    tcp_stream: TcpStream,
    expected_path: String,
    supported: Vec<String>,
    accept_limits: &crate::transport::limits::ConnectionLimits,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, WebSocketError> {
    // tungstenite's handshake-callback contract fixes the error type
    // (`Response<Option<String>>`, the HTTP rejection we must return),
    // so the large `Err` variant is imposed by the external API — it
    // cannot be boxed without breaking the trait it has to satisfy.
    #[allow(clippy::result_large_err)]
    let callback = move |req: &Request, mut resp: Response| {
        let path = req.uri().path();
        if path != expected_path {
            tracing::debug!(
                "[ACCEPT] WebSocket path {} rejected (expected {})",
                path,
                expected_path
            );
            let mut rejection = ErrorResponse::new(Some("Not Found".to_string()));
            *rejection.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
            return Err(rejection);
        }
        if let Some(offered) = req.headers().get("Sec-WebSocket-Protocol") {
            if let Ok(offered) = offered.to_str() {
                if let Some(chosen) = offered
                    .split(',')
                    .map(str::trim)
                    .find(|o| supported.iter().any(|sp| sp == o))
                {
                    if let Ok(value) = chosen.parse() {
                        resp.headers_mut().insert("Sec-WebSocket-Protocol", value);
                    }
                }
            }
        }
        Ok(resp)
    };
    let ws_stream = accept_hdr_async_with_config(
        MaybeTlsStream::Plain(tcp_stream),
        callback,
        Some(ws_protocol_config(&accept_limits.decode_limits())),
    )
    .await?;
    Ok(ws_stream)
}

pub(crate) struct WebSocketClientBuilder<C> {
    config: Option<C>,
    limits: crate::transport::limits::ConnectionLimits,
}

impl<C> WebSocketClientBuilder<C> {
    pub(crate) fn new() -> Self {
        Self {
            config: None,
            limits: crate::transport::limits::ConnectionLimits::default(),
        }
    }

    pub(crate) fn limits(mut self, limits: crate::transport::limits::ConnectionLimits) -> Self {
        self.limits = limits;
        self
    }

    pub(crate) fn config(mut self, config: C) -> Self {
        self.config = Some(config);
        self
    }

    /// Override the target URL on the concrete config. Previously a no-op:
    /// the public factory passed the requested uri into it and the client
    /// silently connected to the config default instead.
    pub(crate) fn target_url<S: Into<String>>(mut self, url: S) -> Self
    where
        C: std::any::Any,
    {
        if let Some(cfg) = self.config.as_mut().and_then(|c| {
            (c as &mut dyn std::any::Any).downcast_mut::<crate::protocol::WebSocketClientConfig>()
        }) {
            cfg.target_url = url.into();
        }
        self
    }

    pub(crate) async fn connect(self) -> Result<WebSocketAdapter<C>, WebSocketError>
    where
        C: crate::protocol::ProtocolConfig,
    {
        let config = self
            .config
            .ok_or_else(|| WebSocketError::Config("Missing WebSocket client config".to_string()))?;

        let ws_cfg = (&config as &dyn std::any::Any)
            .downcast_ref::<crate::protocol::WebSocketClientConfig>()
            .cloned()
            .unwrap_or_default();
        let url = ws_cfg.target_url.clone();

        tracing::debug!("[CONNECT] WebSocket client connecting to: {}", url);

        // Build the handshake request with the configured headers and
        // subprotocol offer (previously both were silently ignored).
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(WebSocketError::Tungstenite)?;
        for (key, value) in &ws_cfg.headers {
            let name: tokio_tungstenite::tungstenite::http::HeaderName = key
                .parse()
                .map_err(|e| WebSocketError::Config(format!("Invalid header name {key}: {e}")))?;
            let value: tokio_tungstenite::tungstenite::http::HeaderValue =
                value.parse().map_err(|e| {
                    WebSocketError::Config(format!("Invalid header value for {key}: {e}"))
                })?;
            request.headers_mut().insert(name, value);
        }
        if !ws_cfg.subprotocols.is_empty() {
            let offer = ws_cfg.subprotocols.join(", ");
            request.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                offer
                    .parse()
                    .map_err(|e| WebSocketError::Config(format!("Invalid subprotocol: {e}")))?,
            );
        }

        // TLS behavior per config — consulted ONLY for wss://, exactly as
        // documented: a plain ws:// connection must neither validate the CA
        // material nor emit the Insecure warning.
        let connector = if request.uri().scheme_str() == Some("wss") {
            Some(build_tls_connector(&ws_cfg.tls)?)
        } else {
            None
        };

        let connect = connect_async_tls_with_config(
            request,
            Some(ws_protocol_config(&self.limits.decode_limits())),
            false,
            connector,
        );
        // Bounded connect (previously connect_timeout was ignored).
        let (ws_stream, _) = if ws_cfg.connect_timeout > std::time::Duration::ZERO {
            tokio::time::timeout(ws_cfg.connect_timeout, connect)
                .await
                .map_err(|_| WebSocketError::Config("WebSocket connect timeout".to_string()))??
        } else {
            connect.await?
        };

        tracing::debug!("[SUCCESS] WebSocket client connected to: {}", url);

        // Create WebSocket adapter
        WebSocketAdapter::new_with_stream(config, ws_stream, self.limits).await
    }
}

#[cfg(test)]
mod frame_policy_tests {
    use super::*;
    use crate::packet::FramePolicy;

    fn classify<C: Send + Sync + 'static>(
        message: Message,
        policy: FramePolicy,
    ) -> MessageProcessResult {
        WebSocketAdapter::<C>::process_websocket_message(
            message,
            policy,
            &crate::transport::limits::ConnectionLimits::default().decode_limits(),
        )
    }

    /// msgtrans is a binary protocol: under the (default) strict policy a
    /// text frame is a protocol error, matching the TypeScript SDK default.
    #[test]
    fn strict_rejects_text_frames() {
        assert_eq!(FramePolicy::default(), FramePolicy::Strict);
        let r = classify::<()>(Message::Text("hello".into()), FramePolicy::Strict);
        assert!(matches!(r, MessageProcessResult::Error(_)));
    }

    #[test]
    fn lenient_wraps_text_frames_as_raw_oneway() {
        let r = classify::<()>(Message::Text("hello".into()), FramePolicy::Lenient);
        match r {
            MessageProcessResult::Packet(p) => {
                assert_eq!(p.payload.as_ref(), b"hello");
                assert_eq!(p.header.packet_type, crate::packet::PacketType::OneWay);
            }
            other => panic!("expected lenient wrap, got {other:?}"),
        }
    }

    #[test]
    fn strict_rejects_undecodable_binary_and_lenient_wraps_it() {
        let junk = bytes::Bytes::from_static(b"not a packet");
        let r = classify::<()>(Message::Binary(junk.clone()), FramePolicy::Strict);
        assert!(matches!(r, MessageProcessResult::Error(_)));
        let r = classify::<()>(Message::Binary(junk), FramePolicy::Lenient);
        assert!(matches!(r, MessageProcessResult::Packet(_)));
    }

    /// A well-formed frame with an invalid packet_type byte is rejected under
    /// strict (TryFrom), not silently delivered as a OneWay.
    #[test]
    fn strict_rejects_invalid_packet_type_byte() {
        let mut bytes = Packet::one_way(1, b"x".to_vec())
            .try_encode()
            .unwrap()
            .to_vec();
        bytes[2] = 9; // invalid packet_type
        let r = classify::<()>(
            Message::Binary(bytes::Bytes::from(bytes)),
            FramePolicy::Strict,
        );
        assert!(matches!(r, MessageProcessResult::Error(_)));
    }

    #[test]
    fn valid_packets_pass_both_policies() {
        for policy in [FramePolicy::Strict, FramePolicy::Lenient] {
            let bytes = Packet::request(3, b"req".to_vec()).try_encode().unwrap();
            let r = classify::<()>(Message::Binary(bytes), policy);
            match r {
                MessageProcessResult::Packet(p) => assert_eq!(p.message_id(), 3),
                other => panic!("expected packet under {policy:?}, got {other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod accept_pump_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// Regression for the Weey :9080 outage: a peer that connects and never
    /// sends the HTTP upgrade must not block the handshakes of peers behind it.
    #[tokio::test]
    async fn silent_peer_does_not_stall_later_handshakes() {
        let mut cfg = crate::protocol::WebSocketServerConfig::default();
        cfg.bind_address = "127.0.0.1:0".parse().unwrap();
        cfg.path = "/gate".to_string();
        let mut server = WebSocketServerBuilder::new()
            .config(cfg)
            .build()
            .await
            .unwrap();
        server.start_pump().await.unwrap();
        let addr = server.local_addr().unwrap();

        // Silent peers: TCP connected, never speak. Before the fix the first
        // one parked `accept()` inside `accept_hdr_async` for good.
        let mut silent = Vec::new();
        for _ in 0..3 {
            silent.push(TcpStream::connect(addr).await.unwrap());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // A well-behaved client must still complete the upgrade promptly.
        let url = format!("ws://{}/gate", addr);
        let client = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .expect("handshake stalled behind silent peers")
        .expect("handshake failed");
        drop(client);

        let accepted = tokio::time::timeout(std::time::Duration::from_secs(3), server.accept())
            .await
            .expect("accept() stalled behind silent peers")
            .expect("accept() failed");
        drop(accepted);

        // Silent peers are cut off once the handshake window elapses; they
        // never reach `accept()`.
        for s in silent.iter_mut() {
            let _ = s.shutdown().await;
        }
        server.shutdown().await.unwrap();
        assert!(server.local_addr().is_err());
    }

    #[test]
    fn handshake_window_is_bounded() {
        assert!(WS_HANDSHAKE_TIMEOUT <= std::time::Duration::from_secs(30));
    }
}

/// [CONFIG] Event-driven QUIC adapter
///
/// This is the modernized version of QUIC adapter, supporting:
/// - Bidirectional stream multiplexing
/// - Event-driven architecture
/// - Read-write separation
/// - Asynchronous queues
use async_trait::async_trait;
use quinn::{
    ClientConfig, ClosedStream, ConnectError, Connection, ConnectionError, Endpoint, ReadError,
    ReadToEndError, ServerConfig, WriteError,
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName},
    DigitallySignedStruct, SignatureScheme,
};
use std::{convert::TryInto, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::{broadcast, mpsc};

use crate::{
    command::ConnectionInfo,
    error::TransportError,
    event::TransportEvent,
    packet::Packet,
    protocol::{AdapterStats, ProtocolAdapter, QuicClientConfig, QuicServerConfig},
    SessionId,
};

#[derive(Debug, thiserror::Error)]
pub enum QuicError {
    #[error("Quinn connection error: {0}")]
    Connect(#[from] ConnectError),

    #[error("Quinn connection error: {0}")]
    Connection(#[from] ConnectionError),

    #[error("Quinn read error: {0}")]
    Read(#[from] ReadError),

    #[error("Quinn write error: {0}")]
    Write(#[from] WriteError),

    #[error("Quinn stream closed: {0}")]
    ClosedStream(#[from] ClosedStream),

    #[error("Quinn read to end error: {0}")]
    ReadToEnd(#[from] ReadToEndError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

impl From<QuicError> for TransportError {
    fn from(error: QuicError) -> Self {
        match error {
            QuicError::Connect(e) => {
                TransportError::connection_error(format!("QUIC connection failed: {}", e), true)
            }
            QuicError::Connection(e) => {
                TransportError::connection_error(format!("QUIC connection error: {}", e), true)
            }
            QuicError::Read(e) => {
                TransportError::connection_error(format!("QUIC read error: {}", e), false)
            }
            QuicError::Write(e) => {
                TransportError::connection_error(format!("QUIC write error: {}", e), false)
            }
            QuicError::ClosedStream(e) => {
                TransportError::connection_error(format!("QUIC stream closed: {}", e), false)
            }
            QuicError::ReadToEnd(e) => {
                TransportError::connection_error(format!("QUIC read to end error: {}", e), false)
            }
            QuicError::Io(e) => {
                TransportError::connection_error(format!("QUIC IO error: {}", e), true)
            }
            QuicError::Tls(e) => TransportError::config_error("quic", format!("TLS error: {}", e)),
            QuicError::ConnectionClosed => {
                TransportError::connection_error("QUIC connection closed", true)
            }
            QuicError::Config(msg) => TransportError::config_error("quic", msg),
            QuicError::Serialization(msg) => TransportError::protocol_error("quic", msg),
        }
    }
}

// Custom verifier that skips server certificate verification
#[derive(Debug)]
struct SkipServerVerification;

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::ED25519,
        ]
    }
}

// Certificate generation function
fn generate_self_signed_cert() -> (CertificateDer<'static>, PrivatePkcs8KeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    (
        cert.cert.der().clone(),
        PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()),
    )
}

/// Configure client without certificate verification (insecure for development)
fn configure_client_insecure() -> ClientConfig {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let mut client_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ));

    let mut transport_config = quinn::TransportConfig::default();
    transport_config.max_idle_timeout(Some(Duration::from_secs(20).try_into().unwrap()));
    client_config.transport_config(Arc::new(transport_config));

    client_config
}

/// Configure client with QuicClientConfig parameters
fn configure_client_with_config(config: &QuicClientConfig) -> Result<ClientConfig, QuicError> {
    let crypto = if config.verify_certificate {
        // Use certificate verification mode
        let mut root_store = rustls::RootCertStore::empty();

        if let Some(ca_cert_pem) = &config.ca_cert_pem {
            // If custom CA certificate is provided, use it
            let cert_bytes = ca_cert_pem.as_bytes();
            let ca_certs = rustls_pemfile::certs(&mut std::io::Cursor::new(cert_bytes))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| QuicError::Config(format!("Failed to parse CA certificate: {}", e)))?;

            for cert in ca_certs {
                root_store.add(cert).map_err(|e| {
                    QuicError::Config(format!("Failed to add CA certificate to store: {}", e))
                })?;
            }

            tracing::debug!(
                "[SECURITY] Using custom CA certificate for QUIC client certificate verification"
            );
        } else {
            // Use system root certificates
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            tracing::debug!("[SECURITY] Using system root certificates for QUIC client certificate verification");
        }

        rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        // Do not verify certificates (insecure mode)
        tracing::debug!(
            "[SECURITY] QUIC client using insecure mode (skip certificate verification)"
        );
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth()
    };

    let mut client_config = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| QuicError::Config(format!("QUIC client config error: {}", e)))?,
    ));

    // Configure transport parameters
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.max_idle_timeout(Some(
        config
            .max_idle_timeout
            .try_into()
            .map_err(|e| QuicError::Config(format!("Invalid idle timeout: {}", e)))?,
    ));

    if let Some(keep_alive) = config.keep_alive_interval {
        transport_config.keep_alive_interval(Some(keep_alive));
    }

    transport_config.initial_rtt(config.initial_rtt);

    // Convert u64 to u32 for VarInt (VarInt only supports From<u32>)
    let max_streams = config.max_concurrent_streams.min(u32::MAX as u64) as u32;
    transport_config.max_concurrent_uni_streams(max_streams.into());
    transport_config.max_concurrent_bidi_streams(max_streams.into());

    client_config.transport_config(Arc::new(transport_config));

    Ok(client_config)
}

/// Configure server with self-signed certificate
fn configure_server_insecure_with_config(
    config: &QuicServerConfig,
) -> (ServerConfig, CertificateDer<'static>) {
    let (cert, key) = generate_self_signed_cert();

    let mut server_config = ServerConfig::with_single_cert(vec![cert.clone()], key.into()).unwrap();

    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.receive_window((1500u32 * 100).into());
    transport_config.max_idle_timeout(Some(config.max_idle_timeout.try_into().unwrap()));

    (server_config, cert)
}

/// Configure server with self-signed certificate (legacy function for backward compatibility)
fn configure_server_insecure() -> (ServerConfig, CertificateDer<'static>) {
    let default_config = QuicServerConfig::default();
    configure_server_insecure_with_config(&default_config)
}

/// Configure server with PEM certificate and key
fn configure_server_with_pem(
    cert_pem: &str,
    key_pem: &str,
    config: &QuicServerConfig,
) -> Result<(ServerConfig, CertificateDer<'static>), QuicError> {
    // Parse private key from PEM string
    let key_bytes = key_pem.as_bytes();
    let key = rustls_pemfile::private_key(&mut std::io::Cursor::new(key_bytes))
        .map_err(|e| QuicError::Config(format!("Failed to parse private key: {}", e)))?
        .ok_or_else(|| QuicError::Config("No private key found in PEM data".to_string()))?;

    // Parse certificate chain from PEM string
    let cert_bytes = cert_pem.as_bytes();
    let certs = rustls_pemfile::certs(&mut std::io::Cursor::new(cert_bytes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| QuicError::Config(format!("Failed to parse certificates: {}", e)))?;

    if certs.is_empty() {
        return Err(QuicError::Config(
            "No certificates found in PEM data".to_string(),
        ));
    }

    // Get the first certificate for return (client verification)
    let first_cert = certs[0].clone();

    // Create server crypto configuration
    let server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| QuicError::Config(format!("TLS configuration error: {}", e)))?;

    // Create QUIC server configuration
    let quic_server_config = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
        .map_err(|e| QuicError::Config(format!("QUIC configuration error: {}", e)))?;

    let mut server_config = ServerConfig::with_crypto(Arc::new(quic_server_config));

    // Configure transport parameters
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.receive_window((1500u32 * 100).into());
    transport_config.max_idle_timeout(Some(config.max_idle_timeout.try_into().unwrap()));

    Ok((server_config, first_cert))
}

/// QUIC protocol adapter (generic support for client and server configurations)
pub struct QuicAdapter<C> {
    /// Session ID (using atomic type for event loop access)
    session_id: Arc<std::sync::atomic::AtomicU64>,
    config: C,
    stats: AdapterStats,
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

impl<C> QuicAdapter<C> {
    pub async fn new_with_connection(
        connection: Connection,
        config: C,
        event_sender: broadcast::Sender<TransportEvent>,
    ) -> Result<Self, QuicError> {
        let session_id = Arc::new(std::sync::atomic::AtomicU64::new(0));

        // Create connection info
        let mut connection_info = ConnectionInfo::default();
        connection_info.protocol = "quic".to_string();
        connection_info.session_id =
            SessionId(session_id.load(std::sync::atomic::Ordering::SeqCst));

        // Get address information
        if let Some(local_addr) = connection.local_ip() {
            connection_info.local_addr = format!("{}:0", local_addr)
                .parse()
                .unwrap_or(connection_info.local_addr);
        }

        // Create communication channels
        let (send_queue_tx, send_queue_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();

        // Start event loop
        let event_loop_handle = Self::start_event_loop(
            connection,
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
    /// This allows clients to subscribe to events sent by QUIC adapter's internal event loop
    pub fn subscribe_events(&self) -> broadcast::Receiver<TransportEvent> {
        self.event_sender.subscribe()
    }

    /// Start event loop based on tokio::select!
    async fn start_event_loop(
        connection: Connection,
        session_id: Arc<std::sync::atomic::AtomicU64>,
        mut send_queue: mpsc::UnboundedReceiver<Packet>,
        mut shutdown_signal: mpsc::UnboundedReceiver<()>,
        event_sender: broadcast::Sender<TransportEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let current_session_id =
                SessionId(session_id.load(std::sync::atomic::Ordering::SeqCst));
            tracing::debug!(
                "[START] QUIC event loop started (session: {})",
                current_session_id
            );

            loop {
                // Get current session ID
                let current_session_id =
                    SessionId(session_id.load(std::sync::atomic::Ordering::SeqCst));

                tokio::select! {
                    // [RECV] Handle incoming data
                    recv_result = connection.accept_uni() => {
                        match recv_result {
                            Ok(mut recv_stream) => {
                                match recv_stream.read_to_end(1024 * 1024).await {
                                    Ok(buf) => {
                                        tracing::debug!("[RECV] QUIC received stream data: {} bytes (session: {})", buf.len(), current_session_id);

                                        // [PERF] Optimization: QUIC stream guarantees data integrity, pre-check to avoid invalid parsing
                                        let packet = if buf.len() < 16 {
                                            // Data too short, cannot be valid Packet, create basic data packet directly
                                            tracing::debug!("[RECV] QUIC data too short, creating basic data packet: {} bytes", buf.len());
                                            Packet::one_way(0, buf)
                                        } else {
                                            // Try to parse as complete Packet
                                            match Packet::from_bytes(&buf) {
                                                Ok(packet) => {
                                                    tracing::debug!("[RECV] QUIC packet parsing successful: {} bytes", packet.payload.len());
                                                    packet
                                                }
                                                Err(e) => {
                                                    tracing::debug!("[RECV] QUIC packet parsing failed: {:?}, creating basic data packet", e);
                                                    // [PERF] Optimization: avoid slice copying, use buf directly
                                                    Packet::one_way(0, buf)
                                                }
                                            }
                                        };

                                        // Send receive event
                                        let event = TransportEvent::MessageReceived(packet);

                                        if let Err(e) = event_sender.send(event) {
                                            tracing::warn!("[RECV] Failed to send receive event: {:?}", e);
                                        }
                                    }
                                    Err(e) => {
                                        // [PERF] Optimization: more fine-grained QUIC error classification handling
                                        let (should_notify, reason, log_level) = match e {
                                            quinn::ReadToEndError::Read(quinn::ReadError::ConnectionLost(_)) => {
                                                (true, crate::error::CloseReason::Normal, "debug")
                                            }
                                            quinn::ReadToEndError::Read(quinn::ReadError::Reset(_)) => {
                                                (true, crate::error::CloseReason::Normal, "debug")
                                            }
                                            quinn::ReadToEndError::TooLong => {
                                                (true, crate::error::CloseReason::Error("QUIC stream too long".to_string()), "warn")
                                            }
                                            _ => {
                                                (true, crate::error::CloseReason::Error(format!("QUIC stream error: {:?}", e)), "error")
                                            }
                                        };

                                        // [PERF] Optimization: more detailed logging
                                        match log_level {
                                            "debug" => tracing::debug!("[CLOSE] QUIC stream closed normally: {:?} (session: {})", e, current_session_id),
                                            "warn" => tracing::warn!("[WARN] QUIC stream warning: {:?} (session: {})", e, current_session_id),
                                            "error" => tracing::error!("[ERROR] QUIC stream error: {:?} (session: {})", e, current_session_id),
                                            _ => {}
                                        }

                                        // Notify upper layer connection closed (network exception or peer closed)
                                        if should_notify {
                                            let close_event = TransportEvent::ConnectionClosed { reason };

                                            if let Err(e) = event_sender.send(close_event) {
                                                tracing::debug!("[CLOSE] Failed to notify upper layer connection closed: session {} - {:?}", current_session_id, e);
                                            } else {
                                                tracing::debug!("[CLOSE] Notified upper layer connection closed: session {}", current_session_id);
                                            }
                                        }

                                        break;
                                    }
                                }
                            }
                            Err(e) => {
                                // [PERF] Optimization: enhanced QUIC connection error classification
                                let (should_notify, reason, log_level) = match e {
                                    quinn::ConnectionError::TimedOut => {
                                        (true, crate::error::CloseReason::Timeout, "info")
                                    }
                                    quinn::ConnectionError::ConnectionClosed(_) => {
                                        (true, crate::error::CloseReason::Normal, "debug")
                                    }
                                    quinn::ConnectionError::ApplicationClosed(_) => {
                                        (true, crate::error::CloseReason::Normal, "debug")
                                    }
                                    quinn::ConnectionError::Reset => {
                                        (true, crate::error::CloseReason::Normal, "debug")
                                    }
                                    quinn::ConnectionError::LocallyClosed => {
                                        (false, crate::error::CloseReason::Normal, "debug") // Local close doesn't need notification
                                    }
                                    _ => {
                                        (true, crate::error::CloseReason::Error(format!("QUIC connection error: {:?}", e)), "error")
                                    }
                                };

                                // [PERF] Optimization: more precise log levels
                                match log_level {
                                    "debug" => tracing::debug!("[CLOSE] QUIC connection closed normally: {:?} (session: {})", e, current_session_id),
                                    "info" => tracing::info!("[TIMEOUT] QUIC connection timeout: {:?} (session: {})", e, current_session_id),
                                    "error" => tracing::error!("[ERROR] QUIC connection error: {:?} (session: {})", e, current_session_id),
                                    _ => {}
                                }

                                // Notify upper layer connection closed (network exception or peer closed)
                                if should_notify {
                                    let close_event = TransportEvent::ConnectionClosed { reason };

                                    if let Err(e) = event_sender.send(close_event) {
                                        tracing::debug!("[CLOSE] Failed to notify upper layer connection closed: session {} - {:?}", current_session_id, e);
                                    } else {
                                        tracing::debug!("[CLOSE] Notified upper layer connection closed: session {}", current_session_id);
                                    }
                                } else {
                                    tracing::debug!("[CLOSE] Local close, no need to notify upper layer (session: {})", current_session_id);
                                }

                                break;
                            }
                        }
                    }

                    // [SEND] Handle outgoing data - optimized version
                    packet = send_queue.recv() => {
                        if let Some(packet) = packet {
                            match connection.open_uni().await {
                                Ok(mut send_stream) => {
                                    // [PERF] Optimization: prepare send data
                                    let data = packet.to_bytes();
                                    let packet_size = packet.payload.len();
                                    let packet_id = packet.header.message_id;

                                    match send_stream.write_all(&data).await {
                                        Ok(_) => {
                                            // [PERF] Optimization: use more efficient stream closing method
                                            match send_stream.finish() {
                                                Ok(_) => {
                                                    tracing::debug!("[SEND] QUIC send successful: {} bytes (ID: {}, session: {})",
                                                        packet_size, packet_id, current_session_id);

                                                    // Send send event
                                                    let event = TransportEvent::MessageSent { packet_id };

                                                    if let Err(e) = event_sender.send(event) {
                                                        tracing::warn!("[SEND] Failed to send send event: {:?}", e);
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::error!("[ERROR] QUIC stream close error: {:?} (session: {})", e, current_session_id);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::error!("[ERROR] QUIC send error: {:?} (session: {})", e, current_session_id);
                                            break;
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("[ERROR] QUIC open send stream error: {:?} (session: {})", e, current_session_id);
                                    break;
                                }
                            }
                        }
                    }

                    // [STOP] Handle shutdown signal
                    _ = shutdown_signal.recv() => {
                        tracing::info!("[STOP] Received shutdown signal, stopping QUIC event loop (session: {})", current_session_id);
                        // Active close: no need to send close event, because it was initiated by upper layer
                        // Lower layer protocol close has already notified peer, upper layer already knows about the close
                        tracing::debug!("[CLOSE] Active close, not sending close event");
                        break;
                    }
                }
            }

            tracing::debug!(
                "[SUCCESS] QUIC event loop ended (session: {})",
                current_session_id
            );
        })
    }
}

// Client adapter implementation
impl QuicAdapter<QuicClientConfig> {
    /// Connect to QUIC server
    pub async fn connect(addr: SocketAddr, config: QuicClientConfig) -> Result<Self, QuicError> {
        tracing::debug!("[CONNECT] QUIC client connecting to: {}", addr);

        // Create client configuration based on config
        let client_config = configure_client_with_config(&config)?;
        let mut endpoint = Endpoint::client(
            config
                .local_bind_address
                .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0))),
        )?;
        endpoint.set_default_client_config(client_config);

        // Use configured server name or default
        let server_name = config.server_name.as_deref().unwrap_or("localhost");

        // Connect to server (using configured timeout)
        let connecting = endpoint.connect(addr, server_name)?;
        let connection = tokio::time::timeout(config.connect_timeout, connecting)
            .await
            .map_err(|_| {
                QuicError::Config(format!(
                    "Connection timeout after {:?}",
                    config.connect_timeout
                ))
            })?
            .map_err(QuicError::Connection)?;
        tracing::debug!(
            "[SUCCESS] QUIC client connected to: {} (server name: {}) timeout: {:?}",
            addr,
            server_name,
            config.connect_timeout
        );

        Self::new_with_connection(connection, config, broadcast::channel(8192).0).await
    }
}

#[async_trait]
impl ProtocolAdapter for QuicAdapter<QuicClientConfig> {
    type Config = QuicClientConfig;
    type Error = QuicError;

    async fn send(&mut self, packet: Packet) -> Result<(), Self::Error> {
        self.send_queue
            .send(packet)
            .map_err(|_| QuicError::ConnectionClosed)?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        tracing::debug!("[CLOSE] Close QUIC client connection");

        // Send shutdown signal
        if let Err(e) = self.shutdown_sender.send(()) {
            tracing::warn!("Failed to send shutdown signal: {:?}", e);
        }

        // Wait for event loop to end
        if let Some(handle) = self.event_loop_handle.take() {
            if let Err(e) = handle.await {
                tracing::warn!("Failed to wait for event loop to end: {:?}", e);
            }
        }

        Ok(())
    }

    fn connection_info(&self) -> ConnectionInfo {
        let mut info = ConnectionInfo::default();
        info.protocol = "quic".to_string();
        info.session_id = SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst));
        info
    }

    fn is_connected(&self) -> bool {
        self.event_loop_handle.is_some()
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
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // QUIC streams auto-flush
        Ok(())
    }
}

// Server adapter implementation
#[async_trait]
impl ProtocolAdapter for QuicAdapter<QuicServerConfig> {
    type Config = QuicServerConfig;
    type Error = QuicError;

    async fn send(&mut self, packet: Packet) -> Result<(), Self::Error> {
        self.send_queue
            .send(packet)
            .map_err(|_| QuicError::ConnectionClosed)?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        tracing::debug!("[CLOSE] Close QUIC server connection");

        // Send shutdown signal
        if let Err(e) = self.shutdown_sender.send(()) {
            tracing::warn!("Failed to send shutdown signal: {:?}", e);
        }

        // Wait for event loop to end
        if let Some(handle) = self.event_loop_handle.take() {
            if let Err(e) = handle.await {
                tracing::warn!("Failed to wait for event loop to end: {:?}", e);
            }
        }

        Ok(())
    }

    fn connection_info(&self) -> ConnectionInfo {
        let mut info = ConnectionInfo::default();
        info.protocol = "quic".to_string();
        info.session_id = SessionId(self.session_id.load(std::sync::atomic::Ordering::SeqCst));
        info
    }

    fn is_connected(&self) -> bool {
        self.event_loop_handle.is_some()
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
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // QUIC streams auto-flush
        Ok(())
    }
}

// Server builder and related structures remain unchanged...
pub(crate) struct QuicServerBuilder {
    config: QuicServerConfig,
    bind_address: Option<SocketAddr>,
}

impl QuicServerBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: QuicServerConfig::default(),
            bind_address: None,
        }
    }

    pub(crate) fn bind_address(mut self, addr: SocketAddr) -> Self {
        self.bind_address = Some(addr);
        self
    }

    pub(crate) fn config(mut self, config: QuicServerConfig) -> Self {
        self.config = config;
        self
    }

    pub(crate) async fn build(self) -> Result<QuicServer, QuicError> {
        let bind_addr = self
            .bind_address
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0)));

        // Choose certificate mode based on configuration
        let server_config = match (&self.config.cert_pem, &self.config.key_pem) {
            (Some(cert_pem), Some(key_pem)) if !cert_pem.is_empty() && !key_pem.is_empty() => {
                // Use provided PEM certificate and private key
                tracing::debug!("[SECURITY] Starting QUIC server with provided PEM certificate");
                let (server_config, _cert) =
                    configure_server_with_pem(cert_pem, key_pem, &self.config)?;
                server_config
            }
            _ => {
                // Use self-signed certificate
                tracing::debug!("[SECURITY] Starting QUIC server with self-signed certificate");
                let (server_config, _cert) = configure_server_insecure_with_config(&self.config);
                server_config
            }
        };

        let endpoint = Endpoint::server(server_config, bind_addr)?;

        tracing::debug!("[START] QUIC server started on: {}", endpoint.local_addr()?);

        Ok(QuicServer {
            config: self.config,
            endpoint,
        })
    }
}

impl Default for QuicServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct QuicServer {
    config: QuicServerConfig,
    endpoint: Endpoint,
}

impl QuicServer {
    pub(crate) fn builder() -> QuicServerBuilder {
        QuicServerBuilder::new()
    }

    pub(crate) async fn accept(&mut self) -> Result<QuicAdapter<QuicServerConfig>, QuicError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or(QuicError::ConnectionClosed)?;
        let connection = incoming.await?;

        tracing::debug!(
            "[SUCCESS] QUIC server accepted connection: {}",
            connection.remote_address()
        );

        QuicAdapter::new_with_connection(
            connection,
            self.config.clone(),
            broadcast::channel(8192).0,
        )
        .await
    }

    pub(crate) fn local_addr(&self) -> Result<SocketAddr, QuicError> {
        self.endpoint.local_addr().map_err(QuicError::Io)
    }
}

pub(crate) struct QuicClientBuilder {
    config: QuicClientConfig,
    target_address: Option<std::net::SocketAddr>,
}

impl QuicClientBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: QuicClientConfig::default(),
            target_address: None,
        }
    }

    pub(crate) fn target_address(mut self, addr: std::net::SocketAddr) -> Self {
        self.target_address = Some(addr);
        self
    }

    pub(crate) fn config(mut self, config: QuicClientConfig) -> Self {
        self.config = config;
        self
    }

    pub(crate) async fn connect(self) -> Result<QuicAdapter<QuicClientConfig>, QuicError> {
        let addr = self
            .target_address
            .ok_or_else(|| QuicError::Config("Target address not set".to_string()))?;
        QuicAdapter::connect(addr, self.config).await
    }
}

impl Default for QuicClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

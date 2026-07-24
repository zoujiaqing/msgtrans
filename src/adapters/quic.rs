/// [CONFIG] Event-driven QUIC adapter
///
/// This is the modernized version of QUIC adapter, supporting:
/// - Bidirectional stream multiplexing
/// - Event-driven architecture
/// - Read-write separation
/// - Asynchronous queues
use async_trait::async_trait;
use quinn::{
    ClientConfig, ClosedStream, ConnectError, Connection as QuinnConnection, ConnectionError,
    Endpoint, ReadError, ReadToEndError, ServerConfig, WriteError,
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName},
    DigitallySignedStruct, SignatureScheme,
};
use std::{convert::TryInto, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::mpsc;

use crate::{
    command::ConnectionInfo,
    connection::Connection,
    error::TransportError,
    event::TransportEvent,
    packet::Packet,
    protocol::{AdapterStats, QuicClientConfig, QuicServerConfig},
    transport::memory_pool::{shared_memory_pool, BufferSize},
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
        // rcgen 0.14 renamed CertifiedKey::key_pair to signing_key.
        PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()),
    )
}

/// Explicit rustls crypto provider for every TLS config this adapter builds.
/// Never rely on the process-level default: it panics at runtime when the
/// consuming binary links more than one provider (ring + aws-lc).
fn ring_client_builder() -> rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier> {
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring provider supports default TLS versions")
}

fn ring_server_builder(
) -> rustls::ConfigBuilder<rustls::ServerConfig, rustls::server::WantsServerCert> {
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring provider supports default TLS versions")
        .with_no_client_auth()
}

/// The msgtrans QUIC ALPN identifier. Set on BOTH sides so a msgtrans
/// endpoint only completes a handshake with another msgtrans endpoint —
/// foreign QUIC clients are rejected at the TLS layer instead of feeding
/// garbage to the framer.
pub(crate) const ALPN_MSGTRANS: &[u8] = b"msgtrans/1";

/// Apply the server transport parameters FROM THE CONFIG — previously the
/// receive window was hardcoded (1500*100) and send_window/initial_rtt/
/// max_concurrent_streams were silently ignored on the server side.
fn apply_server_transport(
    config: &QuicServerConfig,
    server_config: &mut ServerConfig,
) -> Result<(), QuicError> {
    let transport_config =
        Arc::get_mut(&mut server_config.transport).expect("server transport config not yet shared");
    transport_config.receive_window(quinn::VarInt::from_u32(config.receive_window));
    transport_config.send_window(config.send_window as u64);
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
    // Full QUIC varint range, no silent truncation (see the client side).
    let max_streams = quinn::VarInt::from_u64(config.max_concurrent_streams).map_err(|_| {
        QuicError::Config(format!(
            "max_concurrent_streams {} exceeds the QUIC varint range",
            config.max_concurrent_streams
        ))
    })?;
    transport_config.max_concurrent_uni_streams(max_streams);
    transport_config.max_concurrent_bidi_streams(max_streams);
    Ok(())
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

        ring_client_builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    } else {
        // Do not verify certificates (insecure mode)
        tracing::warn!(
            "[SECURITY] QUIC client skipping certificate verification; this is insecure"
        );
        ring_client_builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth()
    };

    let mut crypto = crypto;
    crypto.alpn_protocols = vec![ALPN_MSGTRANS.to_vec()];
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

    // Full QUIC varint range, no silent truncation: an out-of-range value
    // is a configuration error, not a clamp.
    let max_streams = quinn::VarInt::from_u64(config.max_concurrent_streams).map_err(|_| {
        QuicError::Config(format!(
            "max_concurrent_streams {} exceeds the QUIC varint range",
            config.max_concurrent_streams
        ))
    })?;
    transport_config.max_concurrent_uni_streams(max_streams);
    transport_config.max_concurrent_bidi_streams(max_streams);

    client_config.transport_config(Arc::new(transport_config));

    Ok(client_config)
}

/// Configure server with self-signed certificate
fn configure_server_insecure_with_config(
    config: &QuicServerConfig,
) -> Result<(ServerConfig, CertificateDer<'static>), QuicError> {
    let (cert, key) = generate_self_signed_cert();

    let mut server_crypto = ring_server_builder()
        .with_single_cert(vec![cert.clone()], key.into())
        .expect("self-signed cert is valid");
    server_crypto.alpn_protocols = vec![ALPN_MSGTRANS.to_vec()];
    let mut server_config = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .expect("self-signed crypto config is valid"),
    ));

    // The transport parameters come from USER configuration: propagate
    // instead of expecting (an oversized idle timeout used to panic here).
    apply_server_transport(config, &mut server_config)?;

    Ok((server_config, cert))
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
    let mut server_crypto = ring_server_builder()
        .with_single_cert(certs, key)
        .map_err(|e| QuicError::Config(format!("TLS configuration error: {}", e)))?;
    server_crypto.alpn_protocols = vec![ALPN_MSGTRANS.to_vec()];

    // Create QUIC server configuration
    let quic_server_config = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
        .map_err(|e| QuicError::Config(format!("QUIC configuration error: {}", e)))?;

    let mut server_config = ServerConfig::with_crypto(Arc::new(quic_server_config));
    apply_server_transport(config, &mut server_config)?;

    Ok((server_config, first_cert))
}

/// QUIC protocol adapter (generic support for client and server configurations)
pub struct QuicAdapter<C> {
    /// Connection liveness + session id, shared with the event loop.
    state: crate::adapters::core::ConnState,
    // Retained for the generic `C` / diagnostics; not read on the hot path.
    #[allow(dead_code)]
    config: C,
    #[allow(dead_code)]
    stats: AdapterStats,
    #[allow(dead_code)]
    connection_info: ConnectionInfo,
    /// Send queue
    send_queue: mpsc::Sender<crate::adapters::outbound::Outbound>,
    /// Event sender
    event_pipe_rx: Option<crate::adapters::events::EventPipeRx>,
    /// Shutdown signal sender
    shutdown_sender: mpsc::UnboundedSender<()>,
    /// Event loop handle
    event_loop_handle: Option<tokio::task::JoinHandle<()>>,
    /// Frame decode policy (0=Lenient, 1=Strict), shared with the read task.
    frame_policy: Arc<std::sync::atomic::AtomicU8>,
}

impl<C> QuicAdapter<C> {
    pub async fn new_with_connection(
        connection: QuinnConnection,
        config: C,
        is_server: bool,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self, QuicError> {
        let state =
            crate::adapters::core::ConnState::new(crate::adapters::core::ConnStatus::Connecting);

        // Create connection info
        let mut connection_info = ConnectionInfo::default();
        connection_info.protocol = "quic".to_string();
        connection_info.session_id = state.session_id();

        // Get address information
        if let Some(local_addr) = connection.local_ip() {
            connection_info.local_addr = format!("{}:0", local_addr)
                .parse()
                .unwrap_or(connection_info.local_addr);
        }

        // Create communication channels
        let (send_queue_tx, send_queue_rx) = mpsc::channel(limits.outbound_capacity);
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded_channel();
        let frame_policy = Arc::new(std::sync::atomic::AtomicU8::new(
            crate::packet::FramePolicy::default() as u8,
        ));

        // Start event loop
        // Bounded event backbone shared by the supervisor/read/write tasks;
        // whichever ends first publishes the close, and when all clones drop
        // the consumer sees end-of-data.
        let (event_pipe, event_pipe_rx) = crate::adapters::events::event_pipe(limits.pipe_capacity);
        let event_pipe = std::sync::Arc::new(event_pipe);
        let event_loop_handle = Self::start_event_loop(
            connection,
            state.clone(),
            send_queue_rx,
            shutdown_rx,
            event_pipe,
            is_server,
            frame_policy.clone(),
            limits.write_deadline,
        )
        .await;

        Ok(Self {
            state,
            config,
            stats: AdapterStats::new(),
            connection_info,
            send_queue: send_queue_tx,
            event_pipe_rx: Some(event_pipe_rx),
            shutdown_sender: shutdown_tx,
            event_loop_handle: Some(event_loop_handle),
            frame_policy,
        })
    }

    /// Get event stream receiver.
    /// Start event loop with single bidirectional stream multiplexing
    ///
    /// This is the optimized version that uses a single long-lived bidirectional stream
    /// instead of creating a new stream per message. This approach:
    /// - Eliminates stream creation overhead (no `open_uni()` per message)
    /// - Reduces QUIC state machine overhead
    /// - Achieves throughput comparable to TCP
    ///
    /// Frame format: [4-byte length (big-endian)] + [packet data]
    ///
    /// `is_server`: if true, use accept_bi() to wait for client stream; if false, use open_bi() to create stream
    ///
    /// This version uses separate tasks for reading and writing to avoid blocking issues
    /// under high load where one direction could starve the other in a select! loop.
    #[allow(clippy::too_many_arguments)]
    async fn start_event_loop(
        connection: QuinnConnection,
        state: crate::adapters::core::ConnState,
        mut send_queue: mpsc::Receiver<crate::adapters::outbound::Outbound>,
        mut shutdown_signal: mpsc::UnboundedReceiver<()>,
        event_pipe: std::sync::Arc<crate::adapters::events::EventPipe>,
        is_server: bool,
        frame_policy: Arc<std::sync::atomic::AtomicU8>,
        write_deadline: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let current_session_id = state.session_id();
            tracing::debug!(
                "[START] QUIC event loop started with single-stream multiplexing (session: {}, role: {})",
                current_session_id,
                if is_server { "server" } else { "client" }
            );

            // Get bidirectional stream based on role:
            // - Server: accept_bi() to wait for client's stream
            // - Client: open_bi() to create a new stream
            let (send_stream, recv_stream) = if is_server {
                match connection.accept_bi().await {
                    Ok(streams) => streams,
                    Err(e) => {
                        tracing::error!(
                            "[ERROR] Failed to accept bidirectional stream: {:?} (session: {})",
                            e,
                            current_session_id
                        );
                        event_pipe.close(crate::error::CloseReason::Error(format!(
                            "Failed to accept stream: {:?}",
                            e
                        )));
                        return;
                    }
                }
            } else {
                match connection.open_bi().await {
                    Ok(streams) => streams,
                    Err(e) => {
                        tracing::error!(
                            "[ERROR] Failed to open bidirectional stream: {:?} (session: {})",
                            e,
                            current_session_id
                        );
                        event_pipe.close(crate::error::CloseReason::Error(format!(
                            "Failed to open stream: {:?}",
                            e
                        )));
                        return;
                    }
                }
            };

            tracing::debug!(
                "[STREAM] Bidirectional stream ready (session: {})",
                current_session_id
            );

            // The bidi stream is up: the connection is now live.
            state.set_status(crate::adapters::core::ConnStatus::Connected);

            // Use a shared shutdown flag for coordinating between read and write tasks
            let shutdown_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

            // Spawn dedicated READ task
            let read_state = state.clone();
            let read_event_pipe = event_pipe.clone();
            let read_shutdown_flag = shutdown_flag.clone();
            let read_frame_policy = frame_policy.clone();
            let mut read_task = tokio::spawn(async move {
                let mut recv_stream = recv_stream;
                let mut header_buf = [0u8; 4];

                loop {
                    if read_shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }

                    let current_session_id = read_state.session_id();

                    // Read frame header
                    match recv_stream.read_exact(&mut header_buf).await {
                        Ok(_) => {
                            let frame_len = u32::from_be_bytes(header_buf) as usize;

                            // Sanity check
                            if frame_len > 16 * 1024 * 1024 {
                                tracing::error!(
                                    "[ERROR] Frame too large: {} bytes (session: {})",
                                    frame_len,
                                    current_session_id
                                );
                                read_event_pipe.close(crate::error::CloseReason::Error(
                                    "Frame too large".to_string(),
                                ));
                                break;
                            }

                            // Read frame payload into an uninitialized buffer to avoid
                            // zero-filling up to 16 MiB per frame before overwriting it.
                            let mut payload_buf = Vec::<u8>::with_capacity(frame_len);
                            // SAFETY: read_exact writes exactly frame_len bytes before it
                            // returns Ok; on Err we drop payload_buf without ever reading
                            // its (possibly uninitialized) contents.
                            #[allow(clippy::uninit_vec)]
                            unsafe {
                                payload_buf.set_len(frame_len);
                            }
                            match recv_stream.read_exact(&mut payload_buf).await {
                                Ok(_) => {
                                    tracing::debug!(
                                        "[RECV] QUIC received frame: {} bytes (session: {})",
                                        frame_len,
                                        current_session_id
                                    );

                                    let strict = crate::packet::FramePolicy::from(
                                        read_frame_policy
                                            .load(std::sync::atomic::Ordering::Relaxed),
                                    ) == crate::packet::FramePolicy::Strict;
                                    let packet = if payload_buf.len() < 16 {
                                        if strict {
                                            read_event_pipe.close(
                                                crate::error::CloseReason::Error(
                                                    "Undecodable frame (strict policy)".to_string(),
                                                ),
                                            );
                                            break;
                                        }
                                        Packet::one_way(0, payload_buf)
                                    } else {
                                        match Packet::from_bytes(&payload_buf) {
                                            Ok(packet) => packet,
                                            Err(_) if strict => {
                                                read_event_pipe.close(
                                                    crate::error::CloseReason::Error(
                                                        "Undecodable frame (strict policy)"
                                                            .to_string(),
                                                    ),
                                                );
                                                break;
                                            }
                                            Err(_) => Packet::one_way(0, payload_buf),
                                        }
                                    };

                                    // Data plane: backpressure, no loss.
                                    if !read_event_pipe
                                        .deliver(TransportEvent::MessageReceived(packet))
                                        .await
                                    {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "[CLOSE] Failed to read frame payload: {:?} (session: {})",
                                        e,
                                        current_session_id
                                    );
                                    read_event_pipe.close(crate::error::CloseReason::Normal);
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            let current_session_id = read_state.session_id();
                            tracing::debug!(
                                "[CLOSE] Stream read ended: {:?} (session: {})",
                                e,
                                current_session_id
                            );

                            read_event_pipe.close(crate::error::CloseReason::Normal);
                            break;
                        }
                    }
                }
            });

            // Spawn dedicated WRITE task with batching optimization
            let write_state = state.clone();
            let write_event_pipe = event_pipe.clone();
            let write_shutdown_flag = shutdown_flag.clone();
            let mut write_task = tokio::spawn(async move {
                let mut send_stream = send_stream;

                // Batch write optimization constants
                const WRITE_BATCH_SIZE: usize = 32;

                let pool = shared_memory_pool();
                let mut batch = Vec::with_capacity(WRITE_BATCH_SIZE);
                let mut write_buf = pool.get_buffer(BufferSize::Large);

                loop {
                    if write_shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = send_stream.finish();
                        break;
                    }

                    batch.clear();

                    // Batch receive: collect up to WRITE_BATCH_SIZE packets
                    let count = send_queue.recv_many(&mut batch, WRITE_BATCH_SIZE).await;

                    if count == 0 {
                        // send_queue closed
                        tracing::debug!("[CLOSE] Send queue closed");
                        let _ = send_stream.finish();
                        break;
                    }

                    let current_session_id = write_state.session_id();

                    // Batch serialize all packets into write buffer. Per-item
                    // encode failures resolve their OWN completion and are
                    // excluded from the batch (see prepare_quic_batch).
                    write_buf.clear();
                    let (packet_ids, completions) =
                        prepare_quic_batch(batch.drain(..), &mut write_buf);

                    // Single write for entire batch, bounded by the write
                    // deadline: a stalled stream fails the batch instead of
                    // blocking every queued sender indefinitely.
                    let batch_write =
                        tokio::time::timeout(write_deadline, send_stream.write_all(&write_buf))
                            .await;
                    let batch_error = match batch_write {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(format!("Write error: {:?}", e)),
                        Err(_) => Some("QUIC write deadline exceeded".to_string()),
                    };
                    if let Some(reason) = batch_error {
                        tracing::error!(
                            "[ERROR] Failed to write batch: {} (session: {})",
                            reason,
                            current_session_id
                        );
                        // The whole batch failed: every completion in it fails.
                        for completion in completions {
                            completion.complete(Err(
                                crate::error::TransportError::connection_error(
                                    "QUIC write failed",
                                    false,
                                ),
                            ));
                        }
                        write_event_pipe.close(crate::error::CloseReason::Error(reason));
                        break;
                    }

                    tracing::debug!(
                        "[SEND] QUIC batch sent: {} packets, {} bytes (session: {})",
                        packet_ids.len(),
                        write_buf.len(),
                        current_session_id
                    );

                    // Write completions for the whole batch, then diagnostics.
                    for completion in completions {
                        completion.complete(Ok(()));
                    }
                    for packet_id in packet_ids {
                        write_event_pipe.diagnostic(TransportEvent::MessageSent { packet_id });
                    }
                }

                write_buf.clear();
                pool.return_buffer(write_buf, BufferSize::Large);
            });

            // Borrow the handles in select! so the losing task is not orphaned
            // (dropping a JoinHandle by value does not cancel its task).
            // Record which handle select! consumed: a JoinHandle polled after
            // completion panics, so the grace window below must only re-await
            // the tasks that have NOT finished yet.
            let (read_done, write_done) = tokio::select! {
                _ = shutdown_signal.recv() => {
                    tracing::info!("[STOP] Received shutdown signal (session: {})", current_session_id);
                    // Locally initiated close is Normal, not an abnormal end.
                    event_pipe.close(crate::error::CloseReason::Normal);
                    (false, false)
                }
                _ = &mut read_task => {
                    tracing::debug!("[CLOSE] Read task ended (session: {})", current_session_id);
                    (true, false)
                }
                _ = &mut write_task => {
                    tracing::debug!("[CLOSE] Write task ended (session: {})", current_session_id);
                    (false, true)
                }
            };
            // Signal cooperative shutdown, then give the tasks a brief window to
            // observe it and exit cleanly (the write task flushes via finish()
            // before returning). Force-cancel only whatever overruns the window,
            // via TaskGroup, so no task is ever orphaned.
            shutdown_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = tokio::time::timeout(std::time::Duration::from_millis(200), async {
                if !read_done {
                    let _ = (&mut read_task).await;
                }
                if !write_done {
                    let _ = (&mut write_task).await;
                }
            })
            .await;
            let mut tasks = crate::adapters::core::TaskGroup::new();
            tasks.push(read_task);
            tasks.push(write_task);
            tasks.abort_all(); // no-op for tasks that already exited
            state.set_status(crate::adapters::core::ConnStatus::Closed);

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
    pub async fn connect(
        addr: SocketAddr,
        config: QuicClientConfig,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> Result<Self, QuicError> {
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

        Self::new_with_connection(connection, config, false, limits).await
    }
}

#[async_trait]
impl<C: Send + Sync + 'static> Connection for QuicAdapter<C> {
    async fn send_with_completion(
        &mut self,
        packet: Packet,
        completion: crate::connection::WriteCompletion,
    ) -> Result<(), TransportError> {
        crate::adapters::outbound::send_with_completion_bounded(
            &self.send_queue,
            packet,
            completion,
            "quic_outbound_queue",
            "QUIC connection closed",
        )
        .await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        tracing::debug!("[CLOSE] Close QUIC connection");

        if let Err(e) = self.shutdown_sender.send(()) {
            tracing::warn!("Failed to send shutdown signal: {:?}", e);
        }

        if let Some(handle) = self.event_loop_handle.take() {
            if let Err(e) = handle.await {
                tracing::warn!("Failed to wait for event loop to end: {:?}", e);
            }
        }

        Ok(())
    }

    fn session_id(&self) -> SessionId {
        self.state.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        self.state.set_session_id(session_id);
    }

    fn connection_info(&self) -> ConnectionInfo {
        let mut info = ConnectionInfo::default();
        info.protocol = "quic".to_string();
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

// Server builder and related structures remain unchanged...
/// Encode one drained outbound batch into `write_buf`, returning the packet ids
/// and the completions to resolve AFTER the socket write. A packet that fails
/// to encode resolves its OWN completion with the error and is excluded — it is
/// never added to `write_buf` and can never resolve another packet's
/// completion.
fn prepare_quic_batch(
    items: impl IntoIterator<Item = crate::adapters::outbound::Outbound>,
    write_buf: &mut bytes::BytesMut,
) -> (Vec<u32>, Vec<crate::connection::WriteCompletion>) {
    let mut packet_ids = Vec::new();
    let mut completions = Vec::new();
    for item in items {
        let packet = item.packet;
        let completion = item.completion;
        let packet_id = packet.header.message_id;
        let data = match packet.try_encode() {
            Ok(bytes) => bytes,
            Err(e) => {
                if let Some(completion) = completion {
                    completion.complete(Err(crate::error::TransportError::protocol_error(
                        "quic",
                        format!("encode failed: {e}"),
                    )));
                }
                continue;
            }
        };
        if let Some(completion) = completion {
            completions.push(completion);
        }
        packet_ids.push(packet_id);
        let frame_len = data.len() as u32;
        write_buf.extend_from_slice(&frame_len.to_be_bytes());
        write_buf.extend_from_slice(&data);
    }
    (packet_ids, completions)
}

pub(crate) struct QuicServerBuilder {
    config: QuicServerConfig,
    bind_address: Option<SocketAddr>,
    limits: crate::transport::limits::ConnectionLimits,
}

impl QuicServerBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: QuicServerConfig::default(),
            bind_address: None,
            limits: crate::transport::limits::ConnectionLimits::default(),
        }
    }

    pub(crate) fn limits(mut self, limits: crate::transport::limits::ConnectionLimits) -> Self {
        self.limits = limits;
        self
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

        // Choose certificate mode based on configuration. Empty strings are
        // the legacy spelling of "self-signed" (QuicServerConfig::insecure());
        // a PARTIAL pair is a configuration error (validate() rejects it too —
        // this is defense in depth, replacing the old silent fallback).
        let cert_opt = self.config.cert_pem.as_deref().filter(|s| !s.is_empty());
        let key_opt = self.config.key_pem.as_deref().filter(|s| !s.is_empty());
        let server_config = match (cert_opt, key_opt) {
            (Some(cert_pem), Some(key_pem)) => {
                tracing::debug!("[SECURITY] Starting QUIC server with provided PEM certificate");
                let (server_config, _cert) =
                    configure_server_with_pem(cert_pem, key_pem, &self.config)?;
                server_config
            }
            (None, None) => {
                tracing::debug!("[SECURITY] Starting QUIC server with self-signed certificate");
                let (server_config, _cert) = configure_server_insecure_with_config(&self.config)?;
                server_config
            }
            _ => {
                return Err(QuicError::Config(
                    "cert_pem and key_pem must be provided together".to_string(),
                ));
            }
        };

        let endpoint = Endpoint::server(server_config, bind_addr)?;

        tracing::debug!("[START] QUIC server started on: {}", endpoint.local_addr()?);

        Ok(QuicServer {
            config: self.config,
            endpoint,
            limits: self.limits,
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
    limits: crate::transport::limits::ConnectionLimits,
}

impl QuicServer {
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
            true, // is_server = true
            self.limits,
        )
        .await
    }

    pub(crate) fn local_addr(&self) -> Result<SocketAddr, QuicError> {
        self.endpoint.local_addr().map_err(QuicError::Io)
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), QuicError> {
        // Actively close endpoint to release UDP port quickly on shutdown.
        self.endpoint.close(0u32.into(), b"server shutdown");

        // Wait briefly for QUIC internals to drain; don't block shutdown forever.
        let _ = tokio::time::timeout(Duration::from_secs(2), self.endpoint.wait_idle()).await;

        Ok(())
    }
}

pub(crate) struct QuicClientBuilder {
    config: QuicClientConfig,
    target_address: Option<std::net::SocketAddr>,
    limits: crate::transport::limits::ConnectionLimits,
}

impl QuicClientBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: QuicClientConfig::default(),
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

    pub(crate) fn config(mut self, config: QuicClientConfig) -> Self {
        self.config = config;
        self
    }

    pub(crate) async fn connect(self) -> Result<QuicAdapter<QuicClientConfig>, QuicError> {
        let addr = self
            .target_address
            .ok_or_else(|| QuicError::Config("Target address not set".to_string()))?;
        QuicAdapter::connect(addr, self.config, self.limits).await
    }
}

impl Default for QuicClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod batch_tests {
    use super::prepare_quic_batch;
    use crate::adapters::outbound::Outbound;
    use crate::connection::WriteCompletion;
    use crate::packet::Packet;
    use tokio::sync::oneshot;

    fn confirmed(
        packet: Packet,
    ) -> (
        Outbound,
        oneshot::Receiver<Result<(), crate::TransportError>>,
    ) {
        let (tx, rx) = oneshot::channel();
        (
            Outbound {
                packet,
                completion: Some(WriteCompletion::new(Some(tx), None)),
            },
            rx,
        )
    }

    /// Regression: a valid confirmed packet followed by a DETACHED packet that
    /// fails to encode must NOT resolve the confirmed packet's completion. The
    /// earlier pop()-based writer failed the previous packet's receipt while
    /// its bytes stayed in the write buffer and were written — breaking
    /// RespondOutcome::Written.
    #[test]
    fn encode_failure_of_one_item_never_touches_another() {
        // Detached packet with an ext header over u16::MAX: try_encode fails.
        let mut bad = Packet::one_way(2, b"x".to_vec());
        bad.set_ext_header(vec![0u8; u16::MAX as usize + 1]);
        let bad_item = Outbound {
            packet: bad,
            completion: None, // detached
        };

        let (good_item, mut good_rx) = confirmed(Packet::one_way(1, b"ok".to_vec()));

        let mut buf = bytes::BytesMut::new();
        let (ids, completions) = prepare_quic_batch([good_item, bad_item], &mut buf);

        // Only the good packet is in the batch; the bad one is excluded.
        assert_eq!(ids, vec![1]);
        assert_eq!(completions.len(), 1);
        // The good packet's receipt is still PENDING (not mis-failed).
        assert!(
            good_rx.try_recv().is_err(),
            "good receipt must not be resolved by the bad packet"
        );

        // Simulate a successful socket write: resolve the batch completions Ok.
        for completion in completions {
            completion.complete(Ok(()));
        }
        assert!(
            matches!(good_rx.try_recv(), Ok(Ok(()))),
            "good packet must confirm Written"
        );
    }

    /// A confirmed packet that itself fails to encode resolves its OWN receipt
    /// with an error, and does not appear in the batch.
    #[test]
    fn encode_failure_resolves_its_own_receipt() {
        let (good_item, mut good_rx) = confirmed(Packet::one_way(1, b"ok".to_vec()));

        let mut bad = Packet::one_way(2, b"y".to_vec());
        bad.set_ext_header(vec![0u8; u16::MAX as usize + 1]);
        let (bad_item, mut bad_rx) = confirmed(bad);

        let mut buf = bytes::BytesMut::new();
        let (ids, completions) = prepare_quic_batch([good_item, bad_item], &mut buf);

        assert_eq!(ids, vec![1]);
        assert_eq!(completions.len(), 1);
        // Bad packet's own receipt failed immediately.
        assert!(matches!(bad_rx.try_recv(), Ok(Err(_))));
        // Good packet still pending until the write.
        assert!(good_rx.try_recv().is_err());
        for completion in completions {
            completion.complete(Ok(()));
        }
        assert!(matches!(good_rx.try_recv(), Ok(Ok(()))));
    }
}

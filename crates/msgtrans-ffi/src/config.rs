use std::collections::HashMap;
use std::time::Duration;

use crate::error::MsgtransError;

/// Everything needed to build a client.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ClientConfig {
    pub endpoint: Endpoint,
    /// Upper bound on establishing the connection, including the TLS / QUIC
    /// handshake.
    pub connect_timeout_ms: u64,
    pub limits: Option<ClientLimits>,
}

/// Which protocol to speak, and to where. There is deliberately no fallback
/// between protocols: a QUIC endpoint that cannot connect fails, it does not
/// quietly retry over TCP.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum Endpoint {
    /// `address` is `host:port`. Plaintext when `tls` is `None`.
    Tcp {
        address: String,
        tls: Option<TcpTls>,
    },
    /// `url` is `ws://…` or `wss://…`. `tls` is only meaningful for `wss://`;
    /// `None` verifies against the system roots.
    WebSocket {
        url: String,
        tls: Option<WebSocketTls>,
        headers: HashMap<String, String>,
    },
    /// `address` is `host:port`. QUIC is always encrypted; `verification`
    /// decides how the server's identity is checked.
    Quic {
        address: String,
        server_name: Option<String>,
        verification: QuicVerification,
        keep_alive_interval_ms: Option<u64>,
        max_idle_timeout_ms: Option<u64>,
    },
}

/// TLS over TCP. msgtrans authenticates TLS-over-TCP servers by SPKI pin only,
/// so at least one pin is required.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TcpTls {
    /// Base64 SHA-256 of the server certificate's SubjectPublicKeyInfo.
    pub spki_pins: Vec<String>,
    /// SNI and certificate name; defaults to the host part of `address`.
    pub server_name: Option<String>,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum WebSocketTls {
    SystemRoots,
    CustomCa {
        ca_cert_pem: String,
    },
    /// Accept any certificate. Local development only.
    InsecureSkipVerification,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum QuicVerification {
    /// Pin the server's public key (base64 SHA-256 of its SPKI). Works with
    /// self-signed certificates; the handshake signature is still verified.
    SpkiPins { pins: Vec<String> },
    /// Verify against a specific CA.
    CustomCa { ca_cert_pem: String },
    /// Verify against the Mozilla root bundle compiled into the library.
    WebPkiRoots,
    /// Accept any certificate. Local development only.
    InsecureSkipVerification,
}

/// Per-connection limits. `None` keeps the msgtrans default for that field.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ClientLimits {
    pub write_deadline_ms: Option<u64>,
    pub outbound_queue_capacity: Option<u64>,
    pub pipe_capacity: Option<u64>,
    pub max_payload_size: Option<u64>,
    pub max_ext_header_size: Option<u64>,
}

pub(crate) fn builder(
    config: &ClientConfig,
) -> Result<msgtrans::TransportClientBuilder, MsgtransError> {
    let connect_timeout = Duration::from_millis(config.connect_timeout_ms);
    if connect_timeout.is_zero() {
        return Err(MsgtransError::config(
            "connect_timeout_ms",
            "must be greater than zero",
        ));
    }

    let builder = msgtrans::TransportClientBuilder::new();
    let builder = match &config.endpoint {
        Endpoint::Tcp { address, tls } => {
            let mut tcp = msgtrans::TcpClientConfig::new(address)?.connect_timeout(connect_timeout);
            if let Some(tls) = tls {
                // msgtrans enables TLS-over-TCP exactly when pins are present,
                // so an empty list would silently mean plaintext.
                require_pins("endpoint.tls.spki_pins", &tls.spki_pins)?;
                tcp = tcp.spki_pins(tls.spki_pins.iter().cloned());
                if let Some(name) = &tls.server_name {
                    tcp = tcp.server_name(name.clone());
                }
            }
            builder.protocol(tcp.build()?)
        }
        Endpoint::WebSocket { url, tls, headers } => {
            let secure = url.starts_with("wss://");
            if !secure && !url.starts_with("ws://") {
                return Err(MsgtransError::config(
                    "endpoint.url",
                    "must start with ws:// or wss://",
                ));
            }
            let mut ws = msgtrans::WebSocketClientConfig::new(url)?
                .connect_timeout(connect_timeout)
                .headers(headers.clone());
            match (tls, secure) {
                (Some(_), false) => {
                    // msgtrans consults TLS settings only for wss://; accepting
                    // them here would let a caller believe ws:// is protected.
                    return Err(MsgtransError::config(
                        "endpoint.tls",
                        "TLS settings were given for a plaintext ws:// URL",
                    ));
                }
                (Some(tls), true) => {
                    ws = ws.tls(match tls {
                        WebSocketTls::SystemRoots => msgtrans::ClientTls::SystemRoots,
                        WebSocketTls::CustomCa { ca_cert_pem } => {
                            msgtrans::ClientTls::CustomCa(ca_cert_pem.clone())
                        }
                        WebSocketTls::InsecureSkipVerification => msgtrans::ClientTls::Insecure,
                    });
                }
                (None, _) => {}
            }
            builder.protocol(ws.build()?)
        }
        Endpoint::Quic {
            address,
            server_name,
            verification,
            keep_alive_interval_ms,
            max_idle_timeout_ms,
        } => {
            let mut quic =
                msgtrans::QuicClientConfig::new(address)?.connect_timeout(connect_timeout);
            if let Some(name) = server_name {
                quic = quic.server_name(name.clone());
            }
            quic = match verification {
                QuicVerification::SpkiPins { pins } => {
                    // An empty list would leave pinning off and fall back to
                    // WebPKI verification without telling anyone.
                    require_pins("endpoint.verification.pins", pins)?;
                    quic.spki_pins(pins.iter().cloned())
                }
                QuicVerification::CustomCa { ca_cert_pem } => quic.ca_cert_pem(ca_cert_pem.clone()),
                QuicVerification::WebPkiRoots => quic.verify_certificate(true),
                QuicVerification::InsecureSkipVerification => quic.danger_skip_verification(),
            };
            if let Some(ms) = keep_alive_interval_ms {
                quic = quic.keep_alive_interval(Some(Duration::from_millis(*ms)));
            }
            if let Some(ms) = max_idle_timeout_ms {
                quic = quic.max_idle_timeout(Duration::from_millis(*ms));
            }
            builder.protocol(quic.build()?)
        }
    };

    Ok(match &config.limits {
        Some(limits) => builder.limits(client_limits(limits)?),
        None => builder,
    })
}

fn require_pins(field: &str, pins: &[String]) -> Result<(), MsgtransError> {
    if pins.iter().all(|pin| pin.trim().is_empty()) {
        return Err(MsgtransError::config(
            field,
            "at least one SPKI pin is required",
        ));
    }
    Ok(())
}

fn client_limits(limits: &ClientLimits) -> Result<msgtrans::ClientLimits, MsgtransError> {
    let to_usize = |field: &str, value: u64| {
        usize::try_from(value).map_err(|_| MsgtransError::config(field, "value out of range"))
    };
    let mut out = msgtrans::ClientLimits::new();
    if let Some(ms) = limits.write_deadline_ms {
        out = out.write_deadline(Duration::from_millis(ms));
    }
    if let Some(v) = limits.outbound_queue_capacity {
        out = out.outbound_queue_capacity(to_usize("limits.outbound_queue_capacity", v)?);
    }
    if let Some(v) = limits.pipe_capacity {
        out = out.pipe_capacity(to_usize("limits.pipe_capacity", v)?);
    }
    if let Some(v) = limits.max_payload_size {
        out = out.max_payload_size(to_usize("limits.max_payload_size", v)?);
    }
    if let Some(v) = limits.max_ext_header_size {
        out = out.max_ext_header_size(to_usize("limits.max_ext_header_size", v)?);
    }
    Ok(out)
}

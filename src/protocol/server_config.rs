//! Server configuration module - Separated server configuration implementation

#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use crate::protocol::adapter::DynProtocolConfig;
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use crate::protocol::{ConfigError, ProtocolConfig};
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use serde::{Deserialize, Serialize};
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use std::time::Duration;

/// TCP server configuration
#[cfg(feature = "tcp")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpServerConfig {
    /// Bind address
    pub(crate) bind_address: std::net::SocketAddr,
    /// TCP_NODELAY option
    pub(crate) nodelay: bool,
    /// Keepalive time
    pub(crate) keepalive: Option<Duration>,
    /// Connection idle timeout
    pub(crate) idle_timeout: Option<Duration>,
    /// Whether to allow address reuse
    pub(crate) reuse_addr: bool,
}

#[cfg(feature = "tcp")]
impl Default for TcpServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:8080".parse().unwrap(),
            nodelay: true,
            keepalive: Some(Duration::from_secs(60)),
            idle_timeout: Some(Duration::from_secs(300)),
            reuse_addr: true,
        }
    }
}

#[cfg(feature = "tcp")]
impl ProtocolConfig for TcpServerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }
}

#[cfg(feature = "tcp")]
impl DynProtocolConfig for TcpServerConfig {
    fn protocol_name(&self) -> &'static str {
        "tcp"
    }

    fn validate_dyn(&self) -> Result<(), ConfigError> {
        ProtocolConfig::validate(self)
    }
}

/// CONFIG New: Implement server-specific configuration
#[cfg(feature = "tcp")]
impl crate::protocol::adapter::DynServerConfig for TcpServerConfig {
    fn build_server_dyn(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<dyn crate::connection::Server>,
                        crate::error::TransportError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let server = crate::protocol::adapter::ServerConfig::build_server(self, limits).await?;
            Ok(Box::new(server) as Box<dyn crate::connection::Server>)
        })
    }

    fn get_bind_address(&self) -> std::net::SocketAddr {
        self.bind_address
    }

    fn clone_server_dyn(&self) -> Box<dyn crate::protocol::adapter::DynServerConfig> {
        Box::new(self.clone())
    }
}

#[cfg(feature = "tcp")]
impl TcpServerConfig {
    /// Create new TCP server configuration
    pub fn new(bind_address: &str) -> Result<Self, ConfigError> {
        let addr = bind_address
            .parse()
            .map_err(|e| ConfigError::InvalidAddress {
                address: bind_address.to_string(),
                reason: format!("Invalid bind address: {}", e),
                source: Some(Box::new(e)),
            })?;

        Ok(Self {
            bind_address: addr,
            ..Self::default()
        })
    }

    /// Create default configuration (for scenarios requiring default address)
    pub fn default_config() -> Self {
        Self::default()
    }

    /// Set bind address
    pub fn bind_address<A: Into<std::net::SocketAddr>>(mut self, addr: A) -> Self {
        self.bind_address = addr.into();
        self
    }

    /// Set TCP_NODELAY option
    pub fn nodelay(mut self, nodelay: bool) -> Self {
        self.nodelay = nodelay;
        self
    }

    /// Set keepalive time
    pub fn keepalive(mut self, keepalive: Option<Duration>) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// Set connection idle timeout
    pub fn idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Set address reuse
    pub fn reuse_addr(mut self, reuse: bool) -> Self {
        self.reuse_addr = reuse;
        self
    }

    /// Build configuration
    pub fn build(self) -> Result<Self, ConfigError> {
        self.validate()?;
        Ok(self)
    }
}

/// WebSocket server configuration
#[cfg(feature = "websocket")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketServerConfig {
    /// Bind address
    pub(crate) bind_address: std::net::SocketAddr,
    /// WebSocket path
    pub(crate) path: String,
    /// Supported sub-protocols
    pub(crate) subprotocols: Vec<String>,
    /// Ping interval
    pub(crate) ping_interval: Option<Duration>,
    /// Pong timeout
    pub(crate) pong_timeout: Duration,
    /// Connection idle timeout
    pub(crate) idle_timeout: Option<Duration>,
}

#[cfg(feature = "websocket")]
impl Default for WebSocketServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:8080".parse().unwrap(),
            path: "/".to_string(),
            subprotocols: vec![crate::protocol::client_config::WS_SUBPROTOCOL_MSGTRANS.to_string()],
            ping_interval: Some(Duration::from_secs(30)),
            pong_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(300)),
        }
    }
}

#[cfg(feature = "websocket")]
impl ProtocolConfig for WebSocketServerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }
}

#[cfg(feature = "websocket")]
impl DynProtocolConfig for WebSocketServerConfig {
    fn protocol_name(&self) -> &'static str {
        "websocket"
    }

    fn validate_dyn(&self) -> Result<(), ConfigError> {
        ProtocolConfig::validate(self)
    }
}

/// CONFIG New: Implement WebSocket server-specific configuration
#[cfg(feature = "websocket")]
impl crate::protocol::adapter::DynServerConfig for WebSocketServerConfig {
    fn build_server_dyn(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<dyn crate::connection::Server>,
                        crate::error::TransportError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let server = crate::protocol::adapter::ServerConfig::build_server(self, limits).await?;
            Ok(Box::new(server) as Box<dyn crate::connection::Server>)
        })
    }

    fn get_bind_address(&self) -> std::net::SocketAddr {
        self.bind_address
    }

    fn clone_server_dyn(&self) -> Box<dyn crate::protocol::adapter::DynServerConfig> {
        Box::new(self.clone())
    }
}

#[cfg(feature = "websocket")]
impl WebSocketServerConfig {
    /// Create new WebSocket server configuration
    pub fn new(bind_address: &str) -> Result<Self, ConfigError> {
        let addr = bind_address
            .parse()
            .map_err(|e| ConfigError::InvalidAddress {
                address: bind_address.to_string(),
                reason: format!("Invalid bind address: {}", e),
                source: Some(Box::new(e)),
            })?;

        Ok(Self {
            bind_address: addr,
            ..Self::default()
        })
    }

    /// Create default configuration (for scenarios requiring default address)
    pub fn default_config() -> Self {
        Self::default()
    }

    /// Set bind address
    pub fn bind_address<A: Into<std::net::SocketAddr>>(mut self, addr: A) -> Self {
        self.bind_address = addr.into();
        self
    }

    /// Set WebSocket path
    pub fn path<S: Into<String>>(mut self, path: S) -> Self {
        self.path = path.into();
        self
    }

    /// Set supported sub-protocols
    pub fn subprotocols(mut self, protocols: Vec<String>) -> Self {
        self.subprotocols = protocols;
        self
    }

    /// Add sub-protocol
    pub fn add_subprotocol<S: Into<String>>(mut self, protocol: S) -> Self {
        self.subprotocols.push(protocol.into());
        self
    }

    /// Set ping interval
    pub fn ping_interval(mut self, interval: Option<Duration>) -> Self {
        self.ping_interval = interval;
        self
    }

    /// Set pong timeout
    pub fn pong_timeout(mut self, timeout: Duration) -> Self {
        self.pong_timeout = timeout;
        self
    }

    /// Set connection idle timeout
    pub fn idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Build configuration
    pub fn build(self) -> Result<Self, ConfigError> {
        self.validate()?;
        Ok(self)
    }
}

/// QUIC server configuration
#[cfg(feature = "quic")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicServerConfig {
    /// Bind address
    pub(crate) bind_address: std::net::SocketAddr,
    /// TLS certificate PEM content (optional, if None, auto-generate self-signed certificate)
    pub(crate) cert_pem: Option<String>,
    /// TLS private key PEM content (optional, if None, auto-generate self-signed certificate)
    pub(crate) key_pem: Option<String>,
    /// Maximum concurrent streams
    pub(crate) max_concurrent_streams: u64,
    /// Maximum idle timeout
    pub(crate) max_idle_timeout: Duration,
    /// Keep-alive interval
    pub(crate) keep_alive_interval: Option<Duration>,
    /// Initial RTT estimate
    pub(crate) initial_rtt: Duration,
    /// Receive window size
    pub(crate) receive_window: u32,
    /// Send window size
    pub(crate) send_window: u32,
}

#[cfg(feature = "quic")]
impl Default for QuicServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:8080".parse().unwrap(),
            cert_pem: None,
            key_pem: None,
            max_concurrent_streams: 100,
            max_idle_timeout: Duration::from_secs(30),
            keep_alive_interval: Some(Duration::from_secs(15)),
            initial_rtt: Duration::from_millis(100),
            receive_window: 1024 * 1024, // 1MB
            send_window: 1024 * 1024,    // 1MB
        }
    }
}

#[cfg(feature = "quic")]
impl ProtocolConfig for QuicServerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        // Idle timeout must be representable as a QUIC transport parameter —
        // previously an absurd value panicked deep inside the factory path.
        if quinn::IdleTimeout::try_from(self.max_idle_timeout).is_err() {
            return Err(ConfigError::InvalidValue {
                field: "max_idle_timeout".to_string(),
                value: format!("{:?}", self.max_idle_timeout),
                reason: "exceeds the QUIC varint range".to_string(),
                suggestion: "use an idle timeout below ~2^62 milliseconds".to_string(),
            });
        }
        crate::protocol::client_config::validate_quic_stream_count(self.max_concurrent_streams)?;
        // A certificate without its key (or vice versa) is a configuration
        // mistake — previously it silently fell back to a self-signed cert.
        // Empty strings are the legacy spelling of "no PEM" (insecure()).
        let cert = self.cert_pem.as_deref().filter(|s| !s.is_empty());
        let key = self.key_pem.as_deref().filter(|s| !s.is_empty());
        match (cert, key) {
            (Some(_), None) | (None, Some(_)) => {
                return Err(ConfigError::MissingRequiredField {
                    field: "cert_pem/key_pem".to_string(),
                    suggestion: "provide both PEM strings, or neither (self-signed)".to_string(),
                });
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(feature = "quic")]
impl DynProtocolConfig for QuicServerConfig {
    fn protocol_name(&self) -> &'static str {
        "quic"
    }

    fn validate_dyn(&self) -> Result<(), ConfigError> {
        ProtocolConfig::validate(self)
    }
}

/// 🔧 New addition: Implement QUIC server-specific configuration
#[cfg(feature = "quic")]
impl crate::protocol::adapter::DynServerConfig for QuicServerConfig {
    fn build_server_dyn(
        &self,
        limits: crate::transport::limits::ConnectionLimits,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<dyn crate::connection::Server>,
                        crate::error::TransportError,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let server = crate::protocol::adapter::ServerConfig::build_server(self, limits).await?;
            Ok(Box::new(server) as Box<dyn crate::connection::Server>)
        })
    }

    fn get_bind_address(&self) -> std::net::SocketAddr {
        self.bind_address
    }

    fn clone_server_dyn(&self) -> Box<dyn crate::protocol::adapter::DynServerConfig> {
        Box::new(self.clone())
    }
}

#[cfg(feature = "quic")]
impl QuicServerConfig {
    /// Create new QUIC server configuration
    pub fn new(bind_address: &str) -> Result<Self, ConfigError> {
        let addr = bind_address
            .parse()
            .map_err(|e| ConfigError::InvalidAddress {
                address: bind_address.to_string(),
                reason: format!("Invalid bind address: {}", e),
                source: Some(Box::new(e)),
            })?;

        Ok(Self {
            bind_address: addr,
            ..Self::default()
        })
    }

    /// Create default configuration (for scenarios requiring default address)
    pub fn default_config() -> Self {
        Self::default()
    }

    /// Set bind address
    pub fn bind_address<A: Into<std::net::SocketAddr>>(mut self, addr: A) -> Self {
        self.bind_address = addr.into();
        self
    }

    /// Set TLS certificate PEM
    pub fn cert_pem<S: Into<String>>(mut self, cert_pem: S) -> Self {
        self.cert_pem = Some(cert_pem.into());
        self
    }

    /// Set TLS private key PEM
    pub fn key_pem<S: Into<String>>(mut self, key_pem: S) -> Self {
        self.key_pem = Some(key_pem.into());
        self
    }

    /// 设置最大并发流数
    pub fn max_concurrent_streams(mut self, count: u64) -> Self {
        self.max_concurrent_streams = count;
        self
    }

    /// 设置最大空闲超时
    pub fn max_idle_timeout(mut self, timeout: Duration) -> Self {
        self.max_idle_timeout = timeout;
        self
    }

    /// 设置keepalive间隔
    pub fn keep_alive_interval(mut self, interval: Option<Duration>) -> Self {
        self.keep_alive_interval = interval;
        self
    }

    /// 设置初始RTT估值
    pub fn initial_rtt(mut self, rtt: Duration) -> Self {
        self.initial_rtt = rtt;
        self
    }

    /// 设置接收窗口大小
    pub fn receive_window(mut self, window: u32) -> Self {
        self.receive_window = window;
        self
    }

    /// 设置发送窗口大小
    pub fn send_window(mut self, window: u32) -> Self {
        self.send_window = window;
        self
    }

    /// 构建配置
    pub fn build(self) -> Result<Self, ConfigError> {
        self.validate()?;
        Ok(self)
    }

    /// 创建测试用的不安全配置
    pub fn insecure(bind_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(bind_address)?
            .cert_pem("") // 空证书表示使用自签名
            .key_pem("")) // 空私钥表示使用自签名
    }
}

#[cfg(all(test, feature = "websocket", feature = "quic"))]
mod merge_and_validate_tests {
    use super::*;
    use crate::protocol::ProtocolConfig;

    /// User configuration must be REJECTED by validate, never panic later in
    /// the factory path.
    #[test]
    fn quic_validate_rejects_invalid_user_config() {
        let huge_idle = QuicServerConfig::default().max_idle_timeout(Duration::MAX);
        assert!(
            huge_idle.validate().is_err(),
            "oversized idle must be rejected"
        );

        let zero_streams = QuicServerConfig::default().max_concurrent_streams(0);
        assert!(
            zero_streams.validate().is_err(),
            "zero streams must be rejected"
        );

        let partial = QuicServerConfig::default().cert_pem("---CERT---");
        assert!(
            partial.validate().is_err(),
            "cert without key must be rejected"
        );

        // Legacy empty-PEM spelling of self-signed stays valid.
        let legacy = QuicServerConfig::insecure("127.0.0.1:0").expect("cfg");
        assert!(legacy.validate().is_ok(), "empty-PEM self-signed must pass");
        assert!(QuicServerConfig::default().validate().is_ok());
    }

    /// RFC 9000 caps stream-count transport parameters at 2^60. The varint
    /// range goes to 2^62-1, so values in the gap used to pass validation and
    /// then be rejected by the PEER during the handshake.
    #[test]
    fn quic_stream_count_cap_is_rfc9000_2_pow_60_on_both_sides() {
        use crate::protocol::client_config::QuicClientConfig;
        let cap = 1u64 << 60;

        let at_cap = QuicServerConfig::default().max_concurrent_streams(cap);
        assert!(at_cap.validate().is_ok(), "2^60 exactly is legal");
        let over = QuicServerConfig::default().max_concurrent_streams(cap + 1);
        assert!(over.validate().is_err(), "2^60+1 must be rejected");
        let in_gap = QuicServerConfig::default().max_concurrent_streams(1u64 << 61);
        assert!(
            in_gap.validate().is_err(),
            "varint-range-but-over-cap must be rejected"
        );

        // Client side shares the validator (previously it only checked zero).
        let client_over = QuicClientConfig::default().max_concurrent_streams(cap + 1);
        assert!(client_over.validate().is_err());
        let client_zero = QuicClientConfig::default().max_concurrent_streams(0);
        assert!(client_zero.validate().is_err());
        assert!(QuicClientConfig::default().validate().is_ok());
    }
}

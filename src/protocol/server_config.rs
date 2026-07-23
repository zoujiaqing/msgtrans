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

    fn default_config() -> Self {
        Self::default()
    }

    fn merge(mut self, other: Self) -> Self {
        // Simplified merge logic
        if other.bind_address.to_string() != "127.0.0.1:8080" {
            self.bind_address = other.bind_address;
        }
        self.nodelay = other.nodelay;
        if other.keepalive.is_some() {
            self.keepalive = other.keepalive;
        }
        self
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone_dyn(&self) -> Box<dyn DynProtocolConfig> {
        Box::new(self.clone())
    }
}

/// [CONFIG] New: Implement server-specific configuration
#[cfg(feature = "tcp")]
impl crate::protocol::adapter::DynServerConfig for TcpServerConfig {
    fn build_server_dyn(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Server>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let server = crate::protocol::adapter::ServerConfig::build_server(self).await?;
            Ok(Box::new(server) as Box<dyn crate::Server>)
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
    /// Maximum frame size
    pub(crate) max_frame_size: usize,
    /// Maximum message size
    pub(crate) max_message_size: usize,
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
            max_frame_size: 16 * 1024 * 1024,   // 16MB
            max_message_size: 64 * 1024 * 1024, // 64MB
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

    fn default_config() -> Self {
        Self::default()
    }

    // Overlay semantics against the computed default; see the client-side
    // merge for the known limitation (resolved at the #24 API freeze).
    fn merge(mut self, other: Self) -> Self {
        let def = Self::default();
        if other.bind_address != def.bind_address {
            self.bind_address = other.bind_address;
        }
        if other.path != def.path {
            self.path = other.path;
        }
        if other.subprotocols != def.subprotocols {
            self.subprotocols = other.subprotocols;
        }
        if other.max_frame_size != def.max_frame_size {
            self.max_frame_size = other.max_frame_size;
        }
        if other.max_message_size != def.max_message_size {
            self.max_message_size = other.max_message_size;
        }
        if other.ping_interval != def.ping_interval {
            self.ping_interval = other.ping_interval;
        }
        if other.pong_timeout != def.pong_timeout {
            self.pong_timeout = other.pong_timeout;
        }
        if other.idle_timeout != def.idle_timeout {
            self.idle_timeout = other.idle_timeout;
        }
        self
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone_dyn(&self) -> Box<dyn DynProtocolConfig> {
        Box::new(self.clone())
    }
}

/// [CONFIG] New: Implement WebSocket server-specific configuration
#[cfg(feature = "websocket")]
impl crate::protocol::adapter::DynServerConfig for WebSocketServerConfig {
    fn build_server_dyn(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Server>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let server = crate::protocol::adapter::ServerConfig::build_server(self).await?;
            Ok(Box::new(server) as Box<dyn crate::Server>)
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

    /// Set maximum frame size
    pub fn max_frame_size(mut self, size: usize) -> Self {
        self.max_frame_size = size;
        self
    }

    /// Set maximum message size
    pub fn max_message_size(mut self, size: usize) -> Self {
        self.max_message_size = size;
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
        if self.max_concurrent_streams == 0 {
            return Err(ConfigError::InvalidValue {
                field: "max_concurrent_streams".to_string(),
                value: "0".to_string(),
                reason: "a QUIC connection with zero streams cannot carry data".to_string(),
                suggestion: "use at least 1".to_string(),
            });
        }
        if quinn::VarInt::from_u64(self.max_concurrent_streams).is_err() {
            return Err(ConfigError::InvalidValue {
                field: "max_concurrent_streams".to_string(),
                value: self.max_concurrent_streams.to_string(),
                reason: "exceeds the QUIC varint range".to_string(),
                suggestion: "use a value below 2^62".to_string(),
            });
        }
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

    fn default_config() -> Self {
        Self::default()
    }

    // Overlay semantics against the computed default; see the client-side
    // merge for the known limitation (resolved at the #24 API freeze).
    fn merge(mut self, other: Self) -> Self {
        let def = Self::default();
        if other.bind_address != def.bind_address {
            self.bind_address = other.bind_address;
        }
        if other.cert_pem.is_some() {
            self.cert_pem = other.cert_pem;
        }
        if other.key_pem.is_some() {
            self.key_pem = other.key_pem;
        }
        if other.max_concurrent_streams != def.max_concurrent_streams {
            self.max_concurrent_streams = other.max_concurrent_streams;
        }
        if other.max_idle_timeout != def.max_idle_timeout {
            self.max_idle_timeout = other.max_idle_timeout;
        }
        if other.keep_alive_interval != def.keep_alive_interval {
            self.keep_alive_interval = other.keep_alive_interval;
        }
        if other.initial_rtt != def.initial_rtt {
            self.initial_rtt = other.initial_rtt;
        }
        if other.receive_window != def.receive_window {
            self.receive_window = other.receive_window;
        }
        if other.send_window != def.send_window {
            self.send_window = other.send_window;
        }
        self
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone_dyn(&self) -> Box<dyn DynProtocolConfig> {
        Box::new(self.clone())
    }
}

/// 🔧 New addition: Implement QUIC server-specific configuration
#[cfg(feature = "quic")]
impl crate::protocol::adapter::DynServerConfig for QuicServerConfig {
    fn build_server_dyn(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Server>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let server = crate::protocol::adapter::ServerConfig::build_server(self).await?;
            Ok(Box::new(server) as Box<dyn crate::Server>)
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

    /// Overlaying a DEFAULT config must be a no-op: the 1.x merge compared
    /// against stale hardcoded defaults and clobbered customized values.
    #[test]
    fn ws_server_merge_default_overlay_preserves_customization() {
        let custom = WebSocketServerConfig::default()
            .path("/api")
            .max_message_size(4096)
            .ping_interval(None)
            .idle_timeout(Some(Duration::from_secs(7)));
        let merged = custom.clone().merge(WebSocketServerConfig::default());
        assert_eq!(merged.path, "/api");
        assert_eq!(merged.max_message_size, 4096);
        assert_eq!(merged.ping_interval, None);
        assert_eq!(merged.idle_timeout, Some(Duration::from_secs(7)));

        // And a real overlay still wins for the fields it changes.
        let overlay = WebSocketServerConfig::default().max_message_size(9999);
        let merged = custom.merge(overlay);
        assert_eq!(merged.max_message_size, 9999);
        assert_eq!(merged.path, "/api", "untouched fields must survive");
    }

    #[test]
    fn quic_server_merge_covers_transport_fields() {
        let custom = QuicServerConfig::default()
            .receive_window(111)
            .send_window(222)
            .max_concurrent_streams(3);
        let merged = custom.merge(QuicServerConfig::default());
        assert_eq!(merged.receive_window, 111);
        assert_eq!(merged.send_window, 222);
        assert_eq!(merged.max_concurrent_streams, 3);

        let overlay = QuicServerConfig::default().initial_rtt(Duration::from_millis(5));
        let merged = merged.merge(overlay);
        assert_eq!(merged.initial_rtt, Duration::from_millis(5));
        assert_eq!(merged.receive_window, 111);
    }

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
}

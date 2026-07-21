//! Server configuration module - Separated server configuration implementation

use crate::protocol::adapter::DynProtocolConfig;
use crate::protocol::{ConfigError, ProtocolConfig};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// TCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpServerConfig {
    /// Bind address
    pub(crate) bind_address: std::net::SocketAddr,
    /// TCP_NODELAY option
    pub(crate) nodelay: bool,
    /// Keepalive time
    pub(crate) keepalive: Option<Duration>,
    /// Server accept timeout
    pub(crate) accept_timeout: Duration,
    /// Connection idle timeout
    pub(crate) idle_timeout: Option<Duration>,
    /// Whether to allow address reuse
    pub(crate) reuse_addr: bool,
}

impl Default for TcpServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:8080".parse().unwrap(),
            nodelay: true,
            keepalive: Some(Duration::from_secs(60)),
            accept_timeout: Duration::from_secs(30),
            idle_timeout: Some(Duration::from_secs(300)),
            reuse_addr: true,
        }
    }
}

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

    /// Set accept timeout
    pub fn accept_timeout(mut self, timeout: Duration) -> Self {
        self.accept_timeout = timeout;
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

impl Default for WebSocketServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:8080".parse().unwrap(),
            path: "/".to_string(),
            subprotocols: vec![],
            max_frame_size: 16 * 1024 * 1024,   // 16MB
            max_message_size: 64 * 1024 * 1024, // 64MB
            ping_interval: Some(Duration::from_secs(30)),
            pong_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(300)),
        }
    }
}

impl ProtocolConfig for WebSocketServerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }

    fn default_config() -> Self {
        Self::default()
    }

    fn merge(mut self, other: Self) -> Self {
        if other.bind_address.to_string() != "127.0.0.1:8080" {
            self.bind_address = other.bind_address;
        }
        if other.path != "/" {
            self.path = other.path;
        }
        if !other.subprotocols.is_empty() {
            self.subprotocols = other.subprotocols;
        }
        self
    }
}

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

impl ProtocolConfig for QuicServerConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }

    fn default_config() -> Self {
        Self::default()
    }

    fn merge(mut self, other: Self) -> Self {
        if other.bind_address.to_string() != "127.0.0.1:8080" {
            self.bind_address = other.bind_address;
        }
        if other.cert_pem.is_some() {
            self.cert_pem = other.cert_pem;
        }
        if other.key_pem.is_some() {
            self.key_pem = other.key_pem;
        }
        self
    }
}

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

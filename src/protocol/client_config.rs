#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use crate::protocol::adapter::DynProtocolConfig;
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use crate::protocol::{ConfigError, ProtocolConfig};
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use serde::{Deserialize, Serialize};
/// 客户端协议配置
///
/// 专门用于客户端的协议配置，与服务端配置完全分离
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use std::time::Duration;

use crate::{transport::transport::Transport, SessionId, TransportError};
use std::sync::Arc;

/// TCP客户端配置
#[cfg(feature = "tcp")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpClientConfig {
    /// 目标服务器地址
    pub(crate) target_address: std::net::SocketAddr,
    /// 连接超时时间
    pub(crate) connect_timeout: Duration,
    /// TCP_NODELAY选项
    pub(crate) nodelay: bool,
    /// keepalive时间
    pub(crate) keepalive: Option<Duration>,
    /// 本地绑定地址（可选）
    pub(crate) local_bind_address: Option<std::net::SocketAddr>,
}

#[cfg(feature = "tcp")]
impl Default for TcpClientConfig {
    fn default() -> Self {
        Self {
            target_address: "127.0.0.1:80".parse().unwrap(),
            connect_timeout: Duration::from_secs(10),
            nodelay: true,
            keepalive: Some(Duration::from_secs(60)),
            local_bind_address: None,
        }
    }
}
#[cfg(feature = "tcp")]
impl ProtocolConfig for TcpClientConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        Ok(())
    }

    fn default_config() -> Self {
        Self::default()
    }

    fn merge(mut self, other: Self) -> Self {
        if other.target_address.port() != 80 {
            self.target_address = other.target_address;
        }
        if other.connect_timeout != Duration::from_secs(10) {
            self.connect_timeout = other.connect_timeout;
        }
        if !other.nodelay {
            self.nodelay = other.nodelay;
        }
        if other.keepalive.is_some() {
            self.keepalive = other.keepalive;
        }
        if other.local_bind_address.is_some() {
            self.local_bind_address = other.local_bind_address;
        }
        self
    }
}

#[cfg(feature = "tcp")]
impl TcpClientConfig {
    /// 创建新的TCP客户端配置
    pub fn new(target_address: &str) -> Result<Self, ConfigError> {
        let addr = target_address
            .parse()
            .map_err(|e| ConfigError::InvalidAddress {
                address: target_address.to_string(),
                reason: format!("Invalid target address: {}", e),
                source: Some(Box::new(e)),
            })?;

        Ok(Self {
            target_address: addr,
            ..Self::default()
        })
    }

    /// 创建默认配置（用于需要默认地址的场景）
    pub fn default_config() -> Self {
        Self::default()
    }

    /// 设置目标服务器地址
    pub fn target_address<A: Into<std::net::SocketAddr>>(mut self, addr: A) -> Self {
        self.target_address = addr.into();
        self
    }

    /// 从字符串设置目标地址
    pub fn target_str(mut self, addr: &str) -> Result<Self, ConfigError> {
        self.target_address = addr.parse().map_err(|e| ConfigError::InvalidAddress {
            address: addr.to_string(),
            reason: format!("Invalid target address: {}", e),
            source: Some(Box::new(e)),
        })?;
        Ok(self)
    }

    /// 设置连接超时时间
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// 设置TCP_NODELAY选项
    pub fn nodelay(mut self, nodelay: bool) -> Self {
        self.nodelay = nodelay;
        self
    }

    /// 设置keepalive时间
    pub fn keepalive(mut self, keepalive: Option<Duration>) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// 设置本地绑定地址
    pub fn local_bind_address(mut self, addr: Option<std::net::SocketAddr>) -> Self {
        self.local_bind_address = addr;
        self
    }

    /// 构建配置（验证并返回）
    pub fn build(self) -> Result<Self, ConfigError> {
        ProtocolConfig::validate(&self)?;
        Ok(self)
    }

    /// 高性能客户端预设
    pub fn high_performance(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .nodelay(true)
            .connect_timeout(Duration::from_secs(5))
            .keepalive(Some(Duration::from_secs(30))))
    }

    /// 低延迟客户端预设
    pub fn low_latency(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .nodelay(true)
            .connect_timeout(Duration::from_secs(3)))
    }

    /// 可靠连接客户端预设
    pub fn reliable(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .connect_timeout(Duration::from_secs(30))
            .keepalive(Some(Duration::from_secs(120))))
    }
}

/// WebSocket客户端配置
#[cfg(feature = "websocket")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketClientConfig {
    /// 目标服务器URL
    pub(crate) target_url: String,
    /// 连接超时时间
    pub(crate) connect_timeout: Duration,
    /// 请求头
    pub(crate) headers: std::collections::HashMap<String, String>,
    /// 子协议
    pub(crate) subprotocols: Vec<String>,
    /// 最大帧大小
    pub(crate) max_frame_size: usize,
    /// 最大消息大小
    pub(crate) max_message_size: usize,
    /// ping间隔
    pub(crate) ping_interval: Option<Duration>,
    /// pong超时
    pub(crate) pong_timeout: Duration,
    /// TLS 行为(仅对 wss:// 生效)
    pub(crate) tls: ClientTls,
}

/// The msgtrans WebSocket subprotocol identifier: offered by the client and
/// echoed by the server when present. Not required from foreign peers.
#[cfg(feature = "websocket")]
pub const WS_SUBPROTOCOL_MSGTRANS: &str = "msgtrans.v1";

/// TLS behavior for `wss://` WebSocket connections.
///
/// Replaces the 1.x `verify_tls: bool`, which was never wired (the client
/// could not even establish TLS). All variants are now real.
#[cfg(feature = "websocket")]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClientTls {
    /// Verify the server certificate against the bundled webpki roots
    /// (default).
    #[default]
    SystemRoots,
    /// Verify against a custom CA bundle (PEM text) instead of the system
    /// roots — for self-signed / private-PKI deployments.
    CustomCa(String),
    /// Skip certificate verification entirely. Development only.
    Insecure,
}

#[cfg(feature = "websocket")]
impl Default for WebSocketClientConfig {
    fn default() -> Self {
        Self {
            target_url: "ws://localhost:80/".to_string(),
            connect_timeout: Duration::from_secs(10),
            headers: std::collections::HashMap::new(),
            subprotocols: vec![WS_SUBPROTOCOL_MSGTRANS.to_string()],
            // Match the limits that were actually in effect before these
            // knobs were wired (tungstenite's defaults) — wiring must make
            // the options real, not silently tighten them.
            max_frame_size: 16 * 1024 * 1024,
            max_message_size: 64 * 1024 * 1024,
            ping_interval: Some(Duration::from_secs(30)),
            pong_timeout: Duration::from_secs(10),
            tls: ClientTls::default(),
        }
    }
}

#[cfg(feature = "websocket")]
impl ProtocolConfig for WebSocketClientConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if !self.target_url.starts_with("ws://") && !self.target_url.starts_with("wss://") {
            return Err(ConfigError::InvalidValue {
                field: "target_url".to_string(),
                value: self.target_url.clone(),
                reason: "must start with 'ws://' or 'wss://'".to_string(),
                suggestion: "use a valid WebSocket URL".to_string(),
            });
        }

        if self.max_frame_size == 0 {
            return Err(ConfigError::InvalidValue {
                field: "max_frame_size".to_string(),
                value: "0".to_string(),
                reason: "must be > 0".to_string(),
                suggestion: "set a positive value".to_string(),
            });
        }

        if self.max_message_size == 0 {
            return Err(ConfigError::InvalidValue {
                field: "max_message_size".to_string(),
                value: "0".to_string(),
                reason: "must be > 0".to_string(),
                suggestion: "set a positive value".to_string(),
            });
        }

        Ok(())
    }

    fn default_config() -> Self {
        Self::default()
    }

    // Overlay semantics: a field from `other` wins only when it differs from
    // the type's CURRENT default (computed, never hardcoded — the 1.x version
    // compared against stale literal defaults and mis-merged after defaults
    // changed). KNOWN LIMIT: a field explicitly set to its default value (or
    // an Option explicitly set to its default Some/None) cannot be expressed;
    // the #24 API freeze either deletes merge() or replaces it with an
    // Option-per-field overlay type.
    fn merge(mut self, other: Self) -> Self {
        let def = Self::default();
        if other.target_url != def.target_url {
            self.target_url = other.target_url;
        }
        if other.connect_timeout != def.connect_timeout {
            self.connect_timeout = other.connect_timeout;
        }
        if other.headers != def.headers {
            self.headers = other.headers;
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
        if other.tls != def.tls {
            self.tls = other.tls.clone();
        }
        self
    }
}

#[cfg(feature = "websocket")]
impl WebSocketClientConfig {
    /// 创建新的WebSocket客户端配置
    pub fn new(target_url: &str) -> Result<Self, ConfigError> {
        if !target_url.starts_with("ws://") && !target_url.starts_with("wss://") {
            return Err(ConfigError::InvalidValue {
                field: "target_url".to_string(),
                value: target_url.to_string(),
                reason: "must start with 'ws://' or 'wss://'".to_string(),
                suggestion: "use a valid WebSocket URL like 'ws://127.0.0.1:8080/path'".to_string(),
            });
        }

        Ok(Self {
            target_url: target_url.to_string(),
            ..Self::default()
        })
    }

    /// 创建默认配置（用于需要默认URL的场景）
    pub fn default_config() -> Self {
        Self::default()
    }

    /// 设置目标URL
    pub fn target_url<S: Into<String>>(mut self, url: S) -> Self {
        self.target_url = url.into();
        self
    }

    /// 设置连接超时时间
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// 添加请求头
    pub fn header<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// 设置所有请求头
    pub fn headers(mut self, headers: std::collections::HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// 设置子协议
    pub fn subprotocols(mut self, subprotocols: Vec<String>) -> Self {
        self.subprotocols = subprotocols;
        self
    }

    /// 设置最大帧大小
    pub fn max_frame_size(mut self, size: usize) -> Self {
        self.max_frame_size = size;
        self
    }

    /// 设置最大消息大小
    pub fn max_message_size(mut self, size: usize) -> Self {
        self.max_message_size = size;
        self
    }

    /// 设置ping间隔
    pub fn ping_interval(mut self, interval: Option<Duration>) -> Self {
        self.ping_interval = interval;
        self
    }

    /// 设置pong超时
    pub fn pong_timeout(mut self, timeout: Duration) -> Self {
        self.pong_timeout = timeout;
        self
    }

    /// 设置TLS验证
    pub fn tls(mut self, tls: ClientTls) -> Self {
        self.tls = tls;
        self
    }

    /// 构建配置（验证并返回）
    pub fn build(self) -> Result<Self, ConfigError> {
        ProtocolConfig::validate(&self)?;
        Ok(self)
    }

    /// JSON API客户端预设
    pub fn json_api(target_url: &str) -> Result<Self, ConfigError> {
        let mut headers = std::collections::HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());

        Ok(Self::new(target_url)?
            .headers(headers)
            .subprotocols(vec!["json".to_string()])
            .max_frame_size(16 * 1024)
            .max_message_size(512 * 1024))
    }

    /// 实时通信客户端预设
    pub fn realtime(target_url: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_url)?
            .ping_interval(Some(Duration::from_secs(10)))
            .pong_timeout(Duration::from_secs(5))
            .max_frame_size(8 * 1024)
            .connect_timeout(Duration::from_secs(5)))
    }

    /// 文件传输客户端预设
    pub fn file_transfer(target_url: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_url)?
            .max_frame_size(1024 * 1024) // 1MB
            .max_message_size(100 * 1024 * 1024) // 100MB
            .ping_interval(None) // 禁用ping以减少干扰
            .connect_timeout(Duration::from_secs(30)))
    }
}

/// QUIC客户端配置
#[cfg(feature = "quic")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicClientConfig {
    /// 目标服务器地址
    pub(crate) target_address: std::net::SocketAddr,
    /// 服务器名称（用于TLS验证）
    pub(crate) server_name: Option<String>,
    /// 连接超时时间
    pub(crate) connect_timeout: Duration,
    /// 证书验证
    pub(crate) verify_certificate: bool,
    /// 自定义CA证书PEM（可选）
    pub(crate) ca_cert_pem: Option<String>,
    /// 最大并发流数
    pub(crate) max_concurrent_streams: u64,
    /// 最大空闲超时
    pub(crate) max_idle_timeout: Duration,
    /// keepalive间隔
    pub(crate) keep_alive_interval: Option<Duration>,
    /// 初始RTT估值
    pub(crate) initial_rtt: Duration,
    /// 本地绑定地址（可选）
    pub(crate) local_bind_address: Option<std::net::SocketAddr>,
}

#[cfg(feature = "quic")]
impl Default for QuicClientConfig {
    fn default() -> Self {
        Self {
            target_address: "127.0.0.1:443".parse().unwrap(),
            server_name: None,
            connect_timeout: Duration::from_secs(10),
            verify_certificate: true,
            ca_cert_pem: None,
            max_concurrent_streams: 100,
            max_idle_timeout: Duration::from_secs(30),
            keep_alive_interval: Some(Duration::from_secs(15)),
            initial_rtt: Duration::from_millis(100),
            local_bind_address: None,
        }
    }
}

/// RFC 9000 §4.6: stream count transport parameters must not exceed 2^60 —
/// a peer receiving a larger value MUST close the connection. (QUIC varints
/// go up to 2^62-1, so a varint-range check alone is NOT sufficient.)
#[cfg(feature = "quic")]
pub(crate) const QUIC_MAX_STREAM_COUNT: u64 = 1 << 60;

/// Shared 1..=2^60 validator for the client and server QUIC configs.
#[cfg(feature = "quic")]
pub(crate) fn validate_quic_stream_count(value: u64) -> Result<(), ConfigError> {
    if value == 0 {
        return Err(ConfigError::InvalidValue {
            field: "max_concurrent_streams".to_string(),
            value: "0".to_string(),
            reason: "a QUIC connection with zero streams cannot carry data".to_string(),
            suggestion: "use at least 1".to_string(),
        });
    }
    if value > QUIC_MAX_STREAM_COUNT {
        return Err(ConfigError::InvalidValue {
            field: "max_concurrent_streams".to_string(),
            value: value.to_string(),
            reason: "RFC 9000 limits stream count transport parameters to 2^60".to_string(),
            suggestion: "use a value of at most 2^60".to_string(),
        });
    }
    Ok(())
}

#[cfg(feature = "quic")]
impl ProtocolConfig for QuicClientConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        validate_quic_stream_count(self.max_concurrent_streams)?;
        Ok(())
    }

    fn default_config() -> Self {
        Self::default()
    }

    fn merge(mut self, other: Self) -> Self {
        if other.target_address.port() != 443 {
            self.target_address = other.target_address;
        }
        if other.server_name.is_some() {
            self.server_name = other.server_name;
        }
        if other.connect_timeout != Duration::from_secs(10) {
            self.connect_timeout = other.connect_timeout;
        }
        if !other.verify_certificate {
            self.verify_certificate = other.verify_certificate;
        }
        if other.ca_cert_pem.is_some() {
            self.ca_cert_pem = other.ca_cert_pem;
        }
        if other.max_concurrent_streams != 100 {
            self.max_concurrent_streams = other.max_concurrent_streams;
        }
        if other.max_idle_timeout != Duration::from_secs(30) {
            self.max_idle_timeout = other.max_idle_timeout;
        }
        if other.keep_alive_interval.is_some() {
            self.keep_alive_interval = other.keep_alive_interval;
        }
        if other.initial_rtt != Duration::from_millis(100) {
            self.initial_rtt = other.initial_rtt;
        }
        if other.local_bind_address.is_some() {
            self.local_bind_address = other.local_bind_address;
        }
        self
    }
}

#[cfg(feature = "quic")]
impl QuicClientConfig {
    /// 创建新的QUIC客户端配置
    pub fn new(target_address: &str) -> Result<Self, ConfigError> {
        let addr = target_address
            .parse()
            .map_err(|e| ConfigError::InvalidAddress {
                address: target_address.to_string(),
                reason: format!("Invalid target address: {}", e),
                source: Some(Box::new(e)),
            })?;

        Ok(Self {
            target_address: addr,
            ..Self::default()
        })
    }

    /// 创建默认配置（用于需要默认地址的场景）
    pub fn default_config() -> Self {
        Self::default()
    }

    /// 设置目标服务器地址
    pub fn target_address<A: Into<std::net::SocketAddr>>(mut self, addr: A) -> Self {
        self.target_address = addr.into();
        self
    }

    /// 从字符串设置目标地址
    pub fn target_str(mut self, addr: &str) -> Result<Self, ConfigError> {
        self.target_address = addr.parse().map_err(|e| ConfigError::InvalidAddress {
            address: addr.to_string(),
            reason: format!("Invalid target address: {}", e),
            source: Some(Box::new(e)),
        })?;
        Ok(self)
    }

    /// 设置服务器名称（用于TLS验证）
    pub fn server_name<S: Into<String>>(mut self, name: S) -> Self {
        self.server_name = Some(name.into());
        self
    }

    /// 设置连接超时时间
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// 设置证书验证
    pub fn verify_certificate(mut self, verify: bool) -> Self {
        self.verify_certificate = verify;
        self
    }

    /// Explicitly disable certificate verification.
    ///
    /// This is intended for local development and self-signed test servers only.
    pub fn danger_skip_verification(mut self) -> Self {
        tracing::warn!(
            "[SECURITY] QUIC certificate verification disabled; use only for local testing"
        );
        self.verify_certificate = false;
        self
    }

    /// 设置自定义CA证书
    pub fn ca_cert_pem<S: Into<String>>(mut self, cert_pem: S) -> Self {
        self.ca_cert_pem = Some(cert_pem.into());
        self
    }

    /// 设置最大并发流数
    pub fn max_concurrent_streams(mut self, count: u64) -> Self {
        self.max_concurrent_streams = count;
        self
    }

    /// 设置最大空闲时间
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

    /// 设置本地绑定地址
    pub fn local_bind_address(mut self, addr: Option<std::net::SocketAddr>) -> Self {
        self.local_bind_address = addr;
        self
    }

    /// 构建配置（验证并返回）
    pub fn build(self) -> Result<Self, ConfigError> {
        ProtocolConfig::validate(&self)?;
        Ok(self)
    }

    /// 高性能客户端预设
    pub fn high_performance(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .max_concurrent_streams(1000)
            .initial_rtt(Duration::from_millis(20))
            .connect_timeout(Duration::from_secs(5)))
    }

    /// 低延迟客户端预设
    pub fn low_latency(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .initial_rtt(Duration::from_millis(10))
            .keep_alive_interval(Some(Duration::from_secs(5)))
            .max_idle_timeout(Duration::from_secs(10)))
    }

    /// 不安全客户端预设（仅用于测试）
    pub fn insecure(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .danger_skip_verification()
            .server_name("localhost"))
    }
}

#[cfg(feature = "quic")]
impl DynProtocolConfig for QuicClientConfig {
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

/// 🔧 新增：实现 WebSocket 客户端专用配置
#[cfg(feature = "websocket")]
impl crate::protocol::adapter::DynClientConfig for WebSocketClientConfig {
    fn build_connection_dyn(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Connection>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let connection = crate::protocol::adapter::ClientConfig::build_connection(self).await?;
            Ok(Box::new(connection) as Box<dyn crate::Connection>)
        })
    }

    fn get_target_info(&self) -> String {
        self.target_url.clone()
    }

    fn clone_client_dyn(&self) -> Box<dyn crate::protocol::adapter::DynClientConfig> {
        Box::new(self.clone())
    }
}

/// 🔧 新增：实现 TCP 客户端专用配置
#[cfg(feature = "tcp")]
impl crate::protocol::adapter::DynClientConfig for TcpClientConfig {
    fn build_connection_dyn(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Connection>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let connection = crate::protocol::adapter::ClientConfig::build_connection(self).await?;
            Ok(Box::new(connection) as Box<dyn crate::Connection>)
        })
    }

    fn get_target_info(&self) -> String {
        self.target_address.to_string()
    }

    fn clone_client_dyn(&self) -> Box<dyn crate::protocol::adapter::DynClientConfig> {
        Box::new(self.clone())
    }
}

/// 🔧 新增：实现 QUIC 客户端专用配置
#[cfg(feature = "quic")]
impl crate::protocol::adapter::DynClientConfig for QuicClientConfig {
    fn build_connection_dyn(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<Box<dyn crate::Connection>, crate::error::TransportError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            let connection = crate::protocol::adapter::ClientConfig::build_connection(self).await?;
            Ok(Box::new(connection) as Box<dyn crate::Connection>)
        })
    }

    fn get_target_info(&self) -> String {
        self.target_address.to_string()
    }

    fn clone_client_dyn(&self) -> Box<dyn crate::protocol::adapter::DynClientConfig> {
        Box::new(self.clone())
    }
}

#[cfg(feature = "tcp")]
impl DynProtocolConfig for TcpClientConfig {
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

#[cfg(feature = "websocket")]
impl DynProtocolConfig for WebSocketClientConfig {
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

/// 🔧 可连接配置的 trait
pub trait ConnectableConfig {
    async fn connect(self, transport: Arc<Transport>) -> Result<SessionId, TransportError>;
}

#[cfg(feature = "tcp")]
impl ConnectableConfig for TcpClientConfig {
    async fn connect(self, transport: Arc<Transport>) -> Result<SessionId, TransportError> {
        tracing::info!("🔌 TCP 客户端开始连接到 {}", self.target_address);

        let connection = crate::protocol::adapter::ClientConfig::build_connection(&self).await?;

        // 将连接设置到 Transport 中
        let session_id = transport.set_connection(Box::new(connection)).await;
        tracing::info!(
            "✅ TCP 客户端连接成功: {} -> 会话ID: {}",
            self.target_address,
            session_id
        );

        Ok(session_id)
    }
}

#[cfg(feature = "websocket")]
impl ConnectableConfig for WebSocketClientConfig {
    async fn connect(self, transport: Arc<Transport>) -> Result<SessionId, TransportError> {
        tracing::info!("🔌 WebSocket 客户端开始连接到 {}", self.target_url);

        let connection = crate::protocol::adapter::ClientConfig::build_connection(&self).await?;

        // 将连接设置到 Transport 中
        let session_id = transport.set_connection(Box::new(connection)).await;
        tracing::info!(
            "✅ WebSocket 客户端连接成功: {} -> 会话ID: {}",
            self.target_url,
            session_id
        );

        Ok(session_id)
    }
}

#[cfg(feature = "quic")]
impl ConnectableConfig for QuicClientConfig {
    async fn connect(self, transport: Arc<Transport>) -> Result<SessionId, TransportError> {
        tracing::info!("🔌 QUIC 客户端开始连接到 {}", self.target_address);

        let connection = crate::protocol::adapter::ClientConfig::build_connection(&self).await?;

        // 将连接设置到 Transport 中
        let session_id = transport.set_connection(Box::new(connection)).await;
        tracing::info!(
            "✅ QUIC 客户端连接成功: {} -> 会话ID: {}",
            self.target_address,
            session_id
        );

        Ok(session_id)
    }
}

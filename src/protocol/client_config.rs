use crate::protocol::adapter::DynProtocolConfig;
use crate::protocol::{ConfigError, ProtocolConfig};
use serde::{Deserialize, Serialize};
/// 客户端协议配置
///
/// 专门用于客户端的协议配置，与服务端配置完全分离
use std::time::Duration;

use crate::{transport::transport::Transport, SessionId, TransportError};
use std::sync::Arc;

/// TCP客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpClientConfig {
    /// 目标服务器地址
    pub target_address: std::net::SocketAddr,
    /// 连接超时时间
    pub connect_timeout: Duration,
    /// TCP_NODELAY选项
    pub nodelay: bool,
    /// keepalive时间
    pub keepalive: Option<Duration>,
    /// 读缓冲区大小
    pub read_buffer_size: usize,
    /// 写缓冲区大小
    pub write_buffer_size: usize,
    /// 读超时时间
    pub read_timeout: Option<Duration>,
    /// 写超时时间
    pub write_timeout: Option<Duration>,
    /// 重连配置
    pub retry_config: RetryConfig,
    /// 本地绑定地址（可选）
    pub local_bind_address: Option<std::net::SocketAddr>,
}

impl Default for TcpClientConfig {
    fn default() -> Self {
        Self {
            target_address: "127.0.0.1:80".parse().unwrap(),
            connect_timeout: Duration::from_secs(10),
            nodelay: true,
            keepalive: Some(Duration::from_secs(60)),
            read_buffer_size: 8192,
            write_buffer_size: 8192,
            read_timeout: Some(Duration::from_secs(30)),
            write_timeout: Some(Duration::from_secs(30)),
            retry_config: RetryConfig::default(),
            local_bind_address: None,
        }
    }
}

/// 重连配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// 最大重试次数
    pub max_retries: u32,
    /// 重试间隔
    pub retry_interval: Duration,
    /// 指数退避系数
    pub backoff_multiplier: f64,
    /// 最大重试间隔
    pub max_retry_interval: Duration,
    /// 是否启用抖动
    pub jitter: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_interval: Duration::from_millis(500),
            backoff_multiplier: 2.0,
            max_retry_interval: Duration::from_secs(30),
            jitter: true,
        }
    }
}

impl ProtocolConfig for TcpClientConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.read_buffer_size == 0 {
            return Err(ConfigError::InvalidValue {
                field: "read_buffer_size".to_string(),
                value: "0".to_string(),
                reason: "must be > 0".to_string(),
                suggestion: "set a positive value".to_string(),
            });
        }

        if self.write_buffer_size == 0 {
            return Err(ConfigError::InvalidValue {
                field: "write_buffer_size".to_string(),
                value: "0".to_string(),
                reason: "must be > 0".to_string(),
                suggestion: "set a positive value".to_string(),
            });
        }

        if self.retry_config.max_retries > 100 {
            return Err(ConfigError::InvalidValue {
                field: "max_retries".to_string(),
                value: self.retry_config.max_retries.to_string(),
                reason: "excessive retry count may cause resource exhaustion".to_string(),
                suggestion: "use a reasonable value (< 100)".to_string(),
            });
        }

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
        if other.read_buffer_size != 8192 {
            self.read_buffer_size = other.read_buffer_size;
        }
        if other.write_buffer_size != 8192 {
            self.write_buffer_size = other.write_buffer_size;
        }
        if other.read_timeout.is_some() {
            self.read_timeout = other.read_timeout;
        }
        if other.write_timeout.is_some() {
            self.write_timeout = other.write_timeout;
        }
        if other.retry_config.max_retries != 3 {
            self.retry_config = other.retry_config;
        }
        if other.local_bind_address.is_some() {
            self.local_bind_address = other.local_bind_address;
        }
        self
    }
}

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
    pub fn with_target_address<A: Into<std::net::SocketAddr>>(mut self, addr: A) -> Self {
        self.target_address = addr.into();
        self
    }

    /// 从字符串设置目标地址
    pub fn with_target_str(mut self, addr: &str) -> Result<Self, ConfigError> {
        self.target_address = addr.parse().map_err(|e| ConfigError::InvalidAddress {
            address: addr.to_string(),
            reason: format!("Invalid target address: {}", e),
            source: Some(Box::new(e)),
        })?;
        Ok(self)
    }

    /// 设置连接超时时间
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// 设置TCP_NODELAY选项
    pub fn with_nodelay(mut self, nodelay: bool) -> Self {
        self.nodelay = nodelay;
        self
    }

    /// 设置keepalive时间
    pub fn with_keepalive(mut self, keepalive: Option<Duration>) -> Self {
        self.keepalive = keepalive;
        self
    }

    /// 设置读缓冲区大小
    pub fn with_read_buffer_size(mut self, size: usize) -> Self {
        self.read_buffer_size = size;
        self
    }

    /// 设置写缓冲区大小
    pub fn with_write_buffer_size(mut self, size: usize) -> Self {
        self.write_buffer_size = size;
        self
    }

    /// 设置读超时时间
    pub fn with_read_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// 设置写超时时间
    pub fn with_write_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.write_timeout = timeout;
        self
    }

    /// 设置重连配置
    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// 设置本地绑定地址
    pub fn with_local_bind_address(mut self, addr: Option<std::net::SocketAddr>) -> Self {
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
            .with_nodelay(true)
            .with_read_buffer_size(65536)
            .with_write_buffer_size(65536)
            .with_connect_timeout(Duration::from_secs(5))
            .with_keepalive(Some(Duration::from_secs(30))))
    }

    /// 低延迟客户端预设
    pub fn low_latency(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .with_nodelay(true)
            .with_read_buffer_size(4096)
            .with_write_buffer_size(4096)
            .with_connect_timeout(Duration::from_secs(3)))
    }

    /// 可靠连接客户端预设
    pub fn reliable(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .with_retry_config(RetryConfig {
                max_retries: 10,
                retry_interval: Duration::from_secs(1),
                backoff_multiplier: 1.5,
                max_retry_interval: Duration::from_secs(60),
                jitter: true,
            })
            .with_connect_timeout(Duration::from_secs(30))
            .with_keepalive(Some(Duration::from_secs(120))))
    }
}

/// WebSocket客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketClientConfig {
    /// 目标服务器URL
    pub target_url: String,
    /// 连接超时时间
    pub connect_timeout: Duration,
    /// 请求头
    pub headers: std::collections::HashMap<String, String>,
    /// 子协议
    pub subprotocols: Vec<String>,
    /// 最大帧大小
    pub max_frame_size: usize,
    /// 最大消息大小
    pub max_message_size: usize,
    /// ping间隔
    pub ping_interval: Option<Duration>,
    /// pong超时
    pub pong_timeout: Duration,
    /// 重连配置
    pub retry_config: RetryConfig,
    /// TLS验证
    pub verify_tls: bool,
}

impl Default for WebSocketClientConfig {
    fn default() -> Self {
        Self {
            target_url: "ws://localhost:80/".to_string(),
            connect_timeout: Duration::from_secs(10),
            headers: std::collections::HashMap::new(),
            subprotocols: vec![],
            max_frame_size: 64 * 1024,
            max_message_size: 1024 * 1024,
            ping_interval: Some(Duration::from_secs(30)),
            pong_timeout: Duration::from_secs(10),
            retry_config: RetryConfig::default(),
            verify_tls: true,
        }
    }
}

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

    fn merge(mut self, other: Self) -> Self {
        if other.target_url != "ws://localhost:80/" {
            self.target_url = other.target_url;
        }
        if other.connect_timeout != Duration::from_secs(10) {
            self.connect_timeout = other.connect_timeout;
        }
        if !other.headers.is_empty() {
            self.headers = other.headers;
        }
        if !other.subprotocols.is_empty() {
            self.subprotocols = other.subprotocols;
        }
        if other.max_frame_size != 64 * 1024 {
            self.max_frame_size = other.max_frame_size;
        }
        if other.max_message_size != 1024 * 1024 {
            self.max_message_size = other.max_message_size;
        }
        if other.ping_interval.is_some() {
            self.ping_interval = other.ping_interval;
        }
        if other.pong_timeout != Duration::from_secs(10) {
            self.pong_timeout = other.pong_timeout;
        }
        if other.retry_config.max_retries != 3 {
            self.retry_config = other.retry_config;
        }
        if !other.verify_tls {
            self.verify_tls = other.verify_tls;
        }
        self
    }
}

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
    pub fn with_target_url<S: Into<String>>(mut self, url: S) -> Self {
        self.target_url = url.into();
        self
    }

    /// 设置连接超时时间
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// 添加请求头
    pub fn with_header<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// 设置所有请求头
    pub fn with_headers(mut self, headers: std::collections::HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// 设置子协议
    pub fn with_subprotocols(mut self, subprotocols: Vec<String>) -> Self {
        self.subprotocols = subprotocols;
        self
    }

    /// 设置最大帧大小
    pub fn with_max_frame_size(mut self, size: usize) -> Self {
        self.max_frame_size = size;
        self
    }

    /// 设置最大消息大小
    pub fn with_max_message_size(mut self, size: usize) -> Self {
        self.max_message_size = size;
        self
    }

    /// 设置ping间隔
    pub fn with_ping_interval(mut self, interval: Option<Duration>) -> Self {
        self.ping_interval = interval;
        self
    }

    /// 设置pong超时
    pub fn with_pong_timeout(mut self, timeout: Duration) -> Self {
        self.pong_timeout = timeout;
        self
    }

    /// 设置重连配置
    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// 设置TLS验证
    pub fn with_verify_tls(mut self, verify: bool) -> Self {
        self.verify_tls = verify;
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
            .with_headers(headers)
            .with_subprotocols(vec!["json".to_string()])
            .with_max_frame_size(16 * 1024)
            .with_max_message_size(512 * 1024))
    }

    /// 实时通信客户端预设
    pub fn realtime(target_url: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_url)?
            .with_ping_interval(Some(Duration::from_secs(10)))
            .with_pong_timeout(Duration::from_secs(5))
            .with_max_frame_size(8 * 1024)
            .with_connect_timeout(Duration::from_secs(5)))
    }

    /// 文件传输客户端预设
    pub fn file_transfer(target_url: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_url)?
            .with_max_frame_size(1024 * 1024) // 1MB
            .with_max_message_size(100 * 1024 * 1024) // 100MB
            .with_ping_interval(None) // 禁用ping以减少干扰
            .with_connect_timeout(Duration::from_secs(30)))
    }
}

/// QUIC客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicClientConfig {
    /// 目标服务器地址
    pub target_address: std::net::SocketAddr,
    /// 服务器名称（用于TLS验证）
    pub server_name: Option<String>,
    /// 连接超时时间
    pub connect_timeout: Duration,
    /// 证书验证
    pub verify_certificate: bool,
    /// 自定义CA证书PEM（可选）
    pub ca_cert_pem: Option<String>,
    /// 最大并发流数
    pub max_concurrent_streams: u64,
    /// 最大空闲超时
    pub max_idle_timeout: Duration,
    /// keepalive间隔
    pub keep_alive_interval: Option<Duration>,
    /// 初始RTT估值
    pub initial_rtt: Duration,
    /// 重连配置
    pub retry_config: RetryConfig,
    /// 本地绑定地址（可选）
    pub local_bind_address: Option<std::net::SocketAddr>,
}

impl Default for QuicClientConfig {
    fn default() -> Self {
        Self {
            target_address: "127.0.0.1:443".parse().unwrap(),
            server_name: None,
            connect_timeout: Duration::from_secs(10),
            verify_certificate: false, // 默认不验证证书，适合开发环境
            ca_cert_pem: None,
            max_concurrent_streams: 100,
            max_idle_timeout: Duration::from_secs(30),
            keep_alive_interval: Some(Duration::from_secs(15)),
            initial_rtt: Duration::from_millis(100),
            retry_config: RetryConfig::default(),
            local_bind_address: None,
        }
    }
}

impl ProtocolConfig for QuicClientConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.max_concurrent_streams == 0 {
            return Err(ConfigError::InvalidValue {
                field: "max_concurrent_streams".to_string(),
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
        if other.retry_config.max_retries != 3 {
            self.retry_config = other.retry_config;
        }
        if other.local_bind_address.is_some() {
            self.local_bind_address = other.local_bind_address;
        }
        self
    }
}

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
    pub fn with_target_address<A: Into<std::net::SocketAddr>>(mut self, addr: A) -> Self {
        self.target_address = addr.into();
        self
    }

    /// 从字符串设置目标地址
    pub fn with_target_str(mut self, addr: &str) -> Result<Self, ConfigError> {
        self.target_address = addr.parse().map_err(|e| ConfigError::InvalidAddress {
            address: addr.to_string(),
            reason: format!("Invalid target address: {}", e),
            source: Some(Box::new(e)),
        })?;
        Ok(self)
    }

    /// 设置服务器名称（用于TLS验证）
    pub fn with_server_name<S: Into<String>>(mut self, name: S) -> Self {
        self.server_name = Some(name.into());
        self
    }

    /// 设置连接超时时间
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// 设置证书验证
    pub fn with_verify_certificate(mut self, verify: bool) -> Self {
        self.verify_certificate = verify;
        self
    }

    /// 设置自定义CA证书
    pub fn with_ca_cert_pem<S: Into<String>>(mut self, cert_pem: S) -> Self {
        self.ca_cert_pem = Some(cert_pem.into());
        self
    }

    /// 设置最大并发流数
    pub fn with_max_concurrent_streams(mut self, count: u64) -> Self {
        self.max_concurrent_streams = count;
        self
    }

    /// 设置最大空闲时间
    pub fn with_max_idle_timeout(mut self, timeout: Duration) -> Self {
        self.max_idle_timeout = timeout;
        self
    }

    /// 设置keepalive间隔
    pub fn with_keep_alive_interval(mut self, interval: Option<Duration>) -> Self {
        self.keep_alive_interval = interval;
        self
    }

    /// 设置初始RTT估值
    pub fn with_initial_rtt(mut self, rtt: Duration) -> Self {
        self.initial_rtt = rtt;
        self
    }

    /// 设置重连配置
    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// 设置本地绑定地址
    pub fn with_local_bind_address(mut self, addr: Option<std::net::SocketAddr>) -> Self {
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
            .with_max_concurrent_streams(1000)
            .with_initial_rtt(Duration::from_millis(20))
            .with_connect_timeout(Duration::from_secs(5)))
    }

    /// 低延迟客户端预设
    pub fn low_latency(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .with_initial_rtt(Duration::from_millis(10))
            .with_keep_alive_interval(Some(Duration::from_secs(5)))
            .with_max_idle_timeout(Duration::from_secs(10)))
    }

    /// 不安全客户端预设（仅用于测试）
    pub fn insecure(target_address: &str) -> Result<Self, ConfigError> {
        Ok(Self::new(target_address)?
            .with_verify_certificate(false)
            .with_server_name("localhost"))
    }
}

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

impl ConnectableConfig for TcpClientConfig {
    async fn connect(self, transport: Arc<Transport>) -> Result<SessionId, TransportError> {
        tracing::info!("🔌 TCP 客户端开始连接到 {}", self.target_address);

        let session_id = SessionId(1); // 客户端使用固定 session_id
        let connection = crate::protocol::adapter::ClientConfig::build_connection(&self).await?;

        // 将连接设置到 Transport 中
        transport
            .set_connection(Box::new(connection), session_id)
            .await;
        tracing::info!(
            "✅ TCP 客户端连接成功: {} -> 会话ID: {}",
            self.target_address,
            session_id
        );

        Ok(session_id)
    }
}

impl ConnectableConfig for WebSocketClientConfig {
    async fn connect(self, transport: Arc<Transport>) -> Result<SessionId, TransportError> {
        tracing::info!("🔌 WebSocket 客户端开始连接到 {}", self.target_url);

        let session_id = SessionId(1); // 客户端使用固定 session_id
        let connection = crate::protocol::adapter::ClientConfig::build_connection(&self).await?;

        // 将连接设置到 Transport 中
        transport
            .set_connection(Box::new(connection), session_id)
            .await;
        tracing::info!(
            "✅ WebSocket 客户端连接成功: {} -> 会话ID: {}",
            self.target_url,
            session_id
        );

        Ok(session_id)
    }
}

impl ConnectableConfig for QuicClientConfig {
    async fn connect(self, transport: Arc<Transport>) -> Result<SessionId, TransportError> {
        tracing::info!("🔌 QUIC 客户端开始连接到 {}", self.target_address);

        let session_id = SessionId(1); // 客户端使用固定 session_id
        let connection = crate::protocol::adapter::ClientConfig::build_connection(&self).await?;

        // 将连接设置到 Transport 中
        transport
            .set_connection(Box::new(connection), session_id)
            .await;
        tracing::info!(
            "✅ QUIC 客户端连接成功: {} -> 会话ID: {}",
            self.target_address,
            session_id
        );

        Ok(session_id)
    }
}

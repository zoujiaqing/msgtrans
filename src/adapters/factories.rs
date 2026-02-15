use crate::adapters::tcp;
use crate::{
    command::ConnectionInfo,
    connection::{Connection, Server}, // Use unified connection interface
    protocol::{
        ProtocolFactory, QuicClientConfig, QuicServerConfig, TcpClientConfig, TcpServerConfig,
        WebSocketClientConfig, WebSocketServerConfig,
    },
    Packet,
    SessionId,
    TransportError,
};
/// Protocol factory implementation
///
/// Provides factory interface implementation for generic protocol adapters
use async_trait::async_trait;
use std::any::Any;
use std::collections::HashMap;

/// TCP adapter Connection wrapper (client)
pub struct TcpClientConnection {
    inner: tcp::TcpAdapter<TcpClientConfig>,
}

impl TcpClientConnection {
    pub fn new(adapter: tcp::TcpAdapter<TcpClientConfig>) -> Self {
        Self { inner: adapter }
    }
}

#[async_trait]
impl Connection for TcpClientConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.send(packet).await.map_err(Into::into)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.close().await.map_err(Into::into)
    }

    fn is_connected(&self) -> bool {
        use crate::protocol::ProtocolAdapter;
        self.inner.is_connected()
    }

    fn session_id(&self) -> SessionId {
        use crate::protocol::ProtocolAdapter;
        self.inner.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        use crate::protocol::ProtocolAdapter;
        self.inner.set_session_id(session_id);
    }

    fn connection_info(&self) -> ConnectionInfo {
        use crate::protocol::ProtocolAdapter;
        self.inner.connection_info()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.flush().await.map_err(Into::into)
    }

    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        Some(self.inner.subscribe_events())
    }
}

/// TCP server-side connection wrapper
pub struct TcpServerConnection {
    inner: tcp::TcpAdapter<TcpServerConfig>,
}

impl TcpServerConnection {
    pub fn new(adapter: tcp::TcpAdapter<TcpServerConfig>) -> Self {
        Self { inner: adapter }
    }
}

#[async_trait]
impl Connection for TcpServerConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.send(packet).await.map_err(Into::into)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.close().await.map_err(Into::into)
    }

    fn is_connected(&self) -> bool {
        use crate::protocol::ProtocolAdapter;
        self.inner.is_connected()
    }

    fn session_id(&self) -> SessionId {
        use crate::protocol::ProtocolAdapter;
        self.inner.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        use crate::protocol::ProtocolAdapter;
        self.inner.set_session_id(session_id);
    }

    fn connection_info(&self) -> ConnectionInfo {
        use crate::protocol::ProtocolAdapter;
        self.inner.connection_info()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.flush().await.map_err(Into::into)
    }

    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        Some(self.inner.subscribe_events())
    }
}

/// TCP server Server wrapper
pub struct TcpServerWrapper {
    inner: tcp::TcpServer,
}

impl TcpServerWrapper {
    pub fn new(server: tcp::TcpServer) -> Self {
        Self { inner: server }
    }
}

#[async_trait]
impl Server for TcpServerWrapper {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter = self.inner.accept().await.map_err(|e| {
            TransportError::connection_error(format!("TCP accept error: {:?}", e), true)
        })?;

        Ok(Box::new(TcpServerConnection::new(adapter)))
    }

    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        self.inner.local_addr().map_err(Into::into)
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| TransportError::config_error("tcp", &e.to_string()))
    }
}

/// TCP protocol factory
pub struct TcpFactory;

impl TcpFactory {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProtocolFactory for TcpFactory {
    fn protocol_name(&self) -> &'static str {
        "tcp"
    }

    fn supported_schemes(&self) -> Vec<&'static str> {
        vec!["tcp", "tcp4", "tcp6"]
    }

    async fn create_connection(
        &self,
        uri: &str,
        config: Option<Box<dyn Any + Send + Sync>>,
    ) -> Result<Box<dyn Connection>, TransportError> {
        let (addr, _params) = self.parse_uri(uri)?;

        let tcp_config = if let Some(config_box) = config {
            if let Ok(config) = config_box.downcast::<TcpClientConfig>() {
                *config
            } else {
                TcpClientConfig::default()
            }
        } else {
            TcpClientConfig::default()
        };

        let adapter = tcp::TcpAdapter::connect(addr, tcp_config)
            .await
            .map_err(|e| {
                TransportError::connection_error(format!("TCP connection failed: {:?}", e), true)
            })?;

        Ok(Box::new(TcpClientConnection::new(adapter)))
    }

    async fn create_server(
        &self,
        bind_addr: &str,
        config: Option<Box<dyn Any + Send + Sync>>,
    ) -> Result<Box<dyn Server>, TransportError> {
        let addr: std::net::SocketAddr = bind_addr.parse().map_err(|_| {
            TransportError::config_error("protocol", format!("Invalid bind address: {}", bind_addr))
        })?;

        let tcp_config = if let Some(config_box) = config {
            if let Ok(config) = config_box.downcast::<TcpServerConfig>() {
                *config
            } else {
                TcpServerConfig::default()
            }
        } else {
            TcpServerConfig::default()
        };

        let server = tcp::TcpServerBuilder::new()
            .bind_address(addr)
            .config(tcp_config)
            .build()
            .await
            .map_err(|e| {
                TransportError::config_error(
                    "protocol",
                    format!("TCP server creation failed: {:?}", e),
                )
            })?;

        Ok(Box::new(TcpServerWrapper::new(server)))
    }

    fn default_config(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(TcpClientConfig::default())
    }

    fn parse_uri(
        &self,
        uri: &str,
    ) -> Result<(std::net::SocketAddr, HashMap<String, String>), TransportError> {
        // Handle tcp://host:port format
        if let Some(stripped) = uri.strip_prefix("tcp://") {
            if let Ok(addr) = stripped.parse::<std::net::SocketAddr>() {
                Ok((addr, HashMap::new()))
            } else {
                Err(TransportError::config_error(
                    "general",
                    format!("Invalid TCP URI: {}", uri),
                ))
            }
        } else if let Ok(addr) = uri.parse::<std::net::SocketAddr>() {
            // Support host:port format without scheme
            Ok((addr, HashMap::new()))
        } else {
            Err(TransportError::config_error(
                "general",
                format!("Invalid TCP URI: {}", uri),
            ))
        }
    }
}

/// WebSocket and QUIC factories (simplified version, not yet implemented)
pub struct WebSocketFactory;
pub struct QuicFactory;

// Simplified connection wrapper
pub struct WebSocketConnection {
    inner: crate::adapters::websocket::WebSocketAdapter<crate::protocol::WebSocketClientConfig>,
}

impl WebSocketConnection {
    pub fn new(
        adapter: crate::adapters::websocket::WebSocketAdapter<
            crate::protocol::WebSocketClientConfig,
        >,
    ) -> Self {
        Self { inner: adapter }
    }
}

#[async_trait]
impl Connection for WebSocketConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.send(packet).await.map_err(Into::into)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.close().await.map_err(Into::into)
    }

    fn is_connected(&self) -> bool {
        use crate::protocol::ProtocolAdapter;
        self.inner.is_connected()
    }

    fn session_id(&self) -> SessionId {
        use crate::protocol::ProtocolAdapter;
        self.inner.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        use crate::protocol::ProtocolAdapter;
        self.inner.set_session_id(session_id)
    }

    fn connection_info(&self) -> ConnectionInfo {
        use crate::protocol::ProtocolAdapter;
        self.inner.connection_info()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.flush().await.map_err(Into::into)
    }

    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        Some(self.inner.subscribe_events())
    }
}

pub struct WebSocketServerWrapper {
    inner: crate::adapters::websocket::WebSocketServer<crate::protocol::WebSocketServerConfig>,
}

impl WebSocketServerWrapper {
    pub fn new(
        server: crate::adapters::websocket::WebSocketServer<crate::protocol::WebSocketServerConfig>,
    ) -> Self {
        Self { inner: server }
    }
}

pub struct WebSocketServerConnection {
    inner: crate::adapters::websocket::WebSocketAdapter<crate::protocol::WebSocketServerConfig>,
}

impl WebSocketServerConnection {
    pub fn new(
        adapter: crate::adapters::websocket::WebSocketAdapter<
            crate::protocol::WebSocketServerConfig,
        >,
    ) -> Self {
        Self { inner: adapter }
    }
}

#[async_trait]
impl Connection for WebSocketServerConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.send(packet).await.map_err(Into::into)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.close().await.map_err(Into::into)
    }

    fn is_connected(&self) -> bool {
        use crate::protocol::ProtocolAdapter;
        self.inner.is_connected()
    }

    fn session_id(&self) -> SessionId {
        use crate::protocol::ProtocolAdapter;
        self.inner.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        use crate::protocol::ProtocolAdapter;
        self.inner.set_session_id(session_id)
    }

    fn connection_info(&self) -> ConnectionInfo {
        use crate::protocol::ProtocolAdapter;
        self.inner.connection_info()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.flush().await.map_err(Into::into)
    }

    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        Some(self.inner.subscribe_events())
    }
}

#[async_trait]
impl Server for WebSocketServerWrapper {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter = self
            .inner
            .accept()
            .await
            .map_err(|e| TransportError::config_error("websocket", &e.to_string()))?;
        Ok(Box::new(WebSocketServerConnection::new(adapter)))
    }

    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        self.inner
            .local_addr()
            .map_err(|e| TransportError::config_error("websocket", &e.to_string()))
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| TransportError::config_error("websocket", &e.to_string()))
    }
}

pub struct QuicConnection {
    inner: crate::adapters::quic::QuicAdapter<crate::protocol::QuicClientConfig>,
}

impl QuicConnection {
    pub fn new(
        adapter: crate::adapters::quic::QuicAdapter<crate::protocol::QuicClientConfig>,
    ) -> Self {
        Self { inner: adapter }
    }
}

pub struct QuicServerConnection {
    inner: crate::adapters::quic::QuicAdapter<crate::protocol::QuicServerConfig>,
}

impl QuicServerConnection {
    pub fn new(
        adapter: crate::adapters::quic::QuicAdapter<crate::protocol::QuicServerConfig>,
    ) -> Self {
        Self { inner: adapter }
    }
}

#[async_trait]
impl Connection for QuicServerConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.send(packet).await.map_err(Into::into)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.close().await.map_err(Into::into)
    }

    fn is_connected(&self) -> bool {
        use crate::protocol::ProtocolAdapter;
        self.inner.is_connected()
    }

    fn session_id(&self) -> SessionId {
        use crate::protocol::ProtocolAdapter;
        self.inner.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        use crate::protocol::ProtocolAdapter;
        self.inner.set_session_id(session_id)
    }

    fn connection_info(&self) -> ConnectionInfo {
        use crate::protocol::ProtocolAdapter;
        self.inner.connection_info()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.flush().await.map_err(Into::into)
    }

    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        Some(self.inner.subscribe_events())
    }
}

#[async_trait]
impl Connection for QuicConnection {
    async fn send(&mut self, packet: Packet) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.send(packet).await.map_err(Into::into)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.close().await.map_err(Into::into)
    }

    fn is_connected(&self) -> bool {
        use crate::protocol::ProtocolAdapter;
        self.inner.is_connected()
    }

    fn session_id(&self) -> SessionId {
        use crate::protocol::ProtocolAdapter;
        self.inner.session_id()
    }

    fn set_session_id(&mut self, session_id: SessionId) {
        use crate::protocol::ProtocolAdapter;
        self.inner.set_session_id(session_id)
    }

    fn connection_info(&self) -> ConnectionInfo {
        use crate::protocol::ProtocolAdapter;
        self.inner.connection_info()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        use crate::protocol::ProtocolAdapter;
        self.inner.flush().await.map_err(Into::into)
    }

    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        Some(self.inner.subscribe_events())
    }
}

pub struct QuicServerWrapper {
    inner: crate::adapters::quic::QuicServer,
}

impl QuicServerWrapper {
    pub fn new(server: crate::adapters::quic::QuicServer) -> Self {
        Self { inner: server }
    }
}

#[async_trait]
impl Server for QuicServerWrapper {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter = self
            .inner
            .accept()
            .await
            .map_err(|e| TransportError::config_error("quic", &e.to_string()))?;
        Ok(Box::new(QuicServerConnection::new(adapter)))
    }

    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        self.inner
            .local_addr()
            .map_err(|e| TransportError::config_error("quic", &e.to_string()))
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| TransportError::config_error("quic", &e.to_string()))
    }
}

impl WebSocketFactory {
    pub fn new() -> Self {
        Self
    }
}

impl QuicFactory {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ProtocolFactory for WebSocketFactory {
    fn protocol_name(&self) -> &'static str {
        "websocket"
    }
    fn supported_schemes(&self) -> Vec<&'static str> {
        vec!["ws", "wss"]
    }

    async fn create_connection(
        &self,
        uri: &str,
        config: Option<Box<dyn Any + Send + Sync>>,
    ) -> Result<Box<dyn Connection>, TransportError> {
        let ws_config = if let Some(config_box) = config {
            if let Ok(config) = config_box.downcast::<WebSocketClientConfig>() {
                *config
            } else {
                WebSocketClientConfig::default()
            }
        } else {
            WebSocketClientConfig::default()
        };

        let adapter = crate::adapters::websocket::WebSocketClientBuilder::new()
            .target_url(uri)
            .config(ws_config)
            .connect()
            .await
            .map_err(|e| {
                TransportError::connection_error(
                    format!("WebSocket connection failed: {:?}", e),
                    true,
                )
            })?;

        Ok(Box::new(WebSocketConnection::new(adapter)))
    }

    async fn create_server(
        &self,
        bind_addr: &str,
        config: Option<Box<dyn Any + Send + Sync>>,
    ) -> Result<Box<dyn Server>, TransportError> {
        let addr: std::net::SocketAddr = bind_addr.parse().map_err(|_| {
            TransportError::config_error(
                "websocket",
                format!("Invalid bind address: {}", bind_addr),
            )
        })?;

        let ws_config = if let Some(config_box) = config {
            if let Ok(config) = config_box.downcast::<WebSocketServerConfig>() {
                *config
            } else {
                WebSocketServerConfig::default()
            }
        } else {
            WebSocketServerConfig::default()
        };

        let server = crate::adapters::websocket::WebSocketServerBuilder::new()
            .bind_address(addr)
            .config(ws_config)
            .build()
            .await
            .map_err(|e| TransportError::config_error("websocket", &e.to_string()))?;

        Ok(Box::new(WebSocketServerWrapper::new(server)))
    }

    fn default_config(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(WebSocketClientConfig::default())
    }

    fn parse_uri(
        &self,
        uri: &str,
    ) -> Result<(std::net::SocketAddr, HashMap<String, String>), TransportError> {
        // Parse ws://host:port/path or wss://host:port/path format
        if let Ok(url) = uri.parse::<url::Url>() {
            if let Some(host) = url.host_str() {
                let port = url
                    .port()
                    .unwrap_or(if url.scheme() == "wss" { 443 } else { 80 });
                let addr = format!("{}:{}", host, port)
                    .parse::<std::net::SocketAddr>()
                    .map_err(|_| {
                        TransportError::config_error(
                            "websocket",
                            format!("Invalid WebSocket address: {}:{}", host, port),
                        )
                    })?;

                let mut params = HashMap::new();
                params.insert("path".to_string(), url.path().to_string());
                if url.scheme() == "wss" {
                    params.insert("tls".to_string(), "true".to_string());
                }

                Ok((addr, params))
            } else {
                Err(TransportError::config_error(
                    "websocket",
                    format!("Invalid WebSocket URI: {}", uri),
                ))
            }
        } else {
            Err(TransportError::config_error(
                "websocket",
                format!("Invalid WebSocket URI: {}", uri),
            ))
        }
    }
}

#[async_trait]
impl ProtocolFactory for QuicFactory {
    fn protocol_name(&self) -> &'static str {
        "quic"
    }
    fn supported_schemes(&self) -> Vec<&'static str> {
        vec!["quic", "quic+tls"]
    }

    async fn create_connection(
        &self,
        uri: &str,
        config: Option<Box<dyn Any + Send + Sync>>,
    ) -> Result<Box<dyn Connection>, TransportError> {
        let config = if let Some(cfg) = config {
            cfg.downcast::<QuicClientConfig>().map_err(|_| {
                TransportError::config_error("quic", "Invalid QUIC client config type")
            })?
        } else {
            Box::new(QuicClientConfig::default())
        };

        // Parse address from URI or use address from configuration
        let (addr, _) = self.parse_uri(uri)?;

        let adapter = crate::adapters::quic::QuicAdapter::connect(addr, *config)
            .await
            .map_err(|e| TransportError::connection_error(&e.to_string(), true))?;

        Ok(Box::new(QuicConnection::new(adapter)))
    }

    async fn create_server(
        &self,
        bind_addr: &str,
        config: Option<Box<dyn Any + Send + Sync>>,
    ) -> Result<Box<dyn Server>, TransportError> {
        let config = if let Some(cfg) = config {
            cfg.downcast::<QuicServerConfig>().map_err(|_| {
                TransportError::config_error("quic", "Invalid QUIC server config type")
            })?
        } else {
            return Err(TransportError::config_error(
                "quic",
                "QUIC server config is required",
            ));
        };

        let bind_addr: std::net::SocketAddr = bind_addr
            .parse()
            .map_err(|_| TransportError::config_error("quic", "Invalid bind address format"))?;

        let server = crate::adapters::quic::QuicServer::builder()
            .bind_address(bind_addr)
            .config(*config)
            .build()
            .await
            .map_err(|e| TransportError::config_error("quic", &e.to_string()))?;

        Ok(Box::new(QuicServerWrapper::new(server)))
    }

    fn default_config(&self) -> Box<dyn Any + Send + Sync> {
        Box::new(QuicClientConfig::default())
    }

    fn parse_uri(
        &self,
        uri: &str,
    ) -> Result<(std::net::SocketAddr, HashMap<String, String>), TransportError> {
        if let Some(uri_without_scheme) = uri.strip_prefix("quic://") {
            let addr = uri_without_scheme
                .parse::<std::net::SocketAddr>()
                .map_err(|_| TransportError::config_error("quic", "Invalid QUIC URI format"))?;
            Ok((addr, HashMap::new()))
        } else {
            Err(TransportError::config_error(
                "quic",
                "URI must start with quic://",
            ))
        }
    }
}

/// Create standard protocol registry
pub async fn create_standard_registry() -> Result<crate::protocol::ProtocolRegistry, TransportError>
{
    let registry = crate::protocol::ProtocolRegistry::new();

    registry.register(TcpFactory::new()).await?;
    registry.register(WebSocketFactory::new()).await?;
    registry.register(QuicFactory::new()).await?;

    Ok(registry)
}

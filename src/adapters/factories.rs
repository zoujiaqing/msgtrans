#[cfg(feature = "tcp")]
use crate::adapters::tcp;
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use crate::connection::{Connection, Server};
use crate::TransportError;
#[cfg(any(feature = "tcp", feature = "websocket", feature = "quic"))]
use async_trait::async_trait;

/// TCP server Server wrapper
#[cfg(feature = "tcp")]
pub struct TcpServerWrapper {
    inner: tcp::TcpServer,
}

#[cfg(feature = "tcp")]
impl TcpServerWrapper {
    pub fn new(server: tcp::TcpServer) -> Self {
        Self { inner: server }
    }
}

#[cfg(feature = "tcp")]
#[async_trait]
impl Server for TcpServerWrapper {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter = self.inner.accept().await.map_err(|e| {
            TransportError::connection_error(format!("TCP accept error: {:?}", e), true)
        })?;

        Ok(Box::new(adapter))
    }

    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        self.inner.local_addr().map_err(Into::into)
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| TransportError::config_error("tcp", e.to_string()))
    }
}

#[cfg(feature = "websocket")]
pub struct WebSocketServerWrapper {
    inner: crate::adapters::websocket::WebSocketServer<crate::protocol::WebSocketServerConfig>,
}

#[cfg(feature = "websocket")]
impl WebSocketServerWrapper {
    pub fn new(
        server: crate::adapters::websocket::WebSocketServer<crate::protocol::WebSocketServerConfig>,
    ) -> Self {
        Self { inner: server }
    }
}

#[cfg(feature = "websocket")]
#[async_trait]
impl Server for WebSocketServerWrapper {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter = self
            .inner
            .accept()
            .await
            .map_err(|e| TransportError::config_error("websocket", e.to_string()))?;
        Ok(Box::new(adapter))
    }

    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        self.inner
            .local_addr()
            .map_err(|e| TransportError::config_error("websocket", e.to_string()))
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| TransportError::config_error("websocket", e.to_string()))
    }
}

#[cfg(feature = "quic")]
pub struct QuicServerWrapper {
    inner: crate::adapters::quic::QuicServer,
}

#[cfg(feature = "quic")]
impl QuicServerWrapper {
    pub fn new(server: crate::adapters::quic::QuicServer) -> Self {
        Self { inner: server }
    }
}

#[cfg(feature = "quic")]
#[async_trait]
impl Server for QuicServerWrapper {
    async fn accept(&mut self) -> Result<Box<dyn Connection>, TransportError> {
        let adapter = self
            .inner
            .accept()
            .await
            .map_err(|e| TransportError::config_error("quic", e.to_string()))?;
        Ok(Box::new(adapter))
    }

    fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        self.inner
            .local_addr()
            .map_err(|e| TransportError::config_error("quic", e.to_string()))
    }

    async fn shutdown(&mut self) -> Result<(), TransportError> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| TransportError::config_error("quic", e.to_string()))
    }
}

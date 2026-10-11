/// Server-side transport layer module
///
/// Provides transport layer API specifically for server-side listening
use crate::{error::TransportError, transport::config::TransportConfig};

// Import new TransportServer
use super::transport_server::TransportServer;

/// Server-side transport builder - focused on server listening related configuration
pub struct TransportServerBuilder {
    transport_config: TransportConfig,
    /// Protocol configuration storage - server supports multi-protocol listening
    protocol_configs:
        std::collections::HashMap<String, Box<dyn crate::protocol::adapter::DynServerConfig>>,
    /// Buffer size for actor channels
    actor_buffer_size: Option<usize>,
    /// Frame decode policy applied to accepted connections.
    frame_policy: crate::packet::FramePolicy,
    /// Hard cap on concurrent sessions (None = unlimited).
    max_connections: Option<usize>,
}

impl TransportServerBuilder {
    pub fn new() -> Self {
        Self {
            transport_config: TransportConfig::default(),
            protocol_configs: std::collections::HashMap::new(),
            actor_buffer_size: None,
            frame_policy: crate::packet::FramePolicy::default(),
            max_connections: None,
        }
    }

    /// Cap the number of concurrent sessions across all protocols
    /// (default: unlimited).
    ///
    /// A connection accepted while the server is at capacity is closed
    /// immediately, before any per-session resources are allocated.
    pub fn max_connections(mut self, max: usize) -> Self {
        self.max_connections = Some(max);
        self
    }

    /// Set the frame decode policy applied to accepted connections (default Lenient).
    ///
    /// Under `Strict`, WebSocket/QUIC connections that receive an undecodable
    /// frame are closed instead of downgrading it to a raw one-way message.
    pub fn frame_policy(mut self, policy: crate::packet::FramePolicy) -> Self {
        self.frame_policy = policy;
        self
    }

    /// Set transport layer base configuration
    pub fn transport_config(mut self, config: TransportConfig) -> Self {
        self.transport_config = config;
        self
    }

    /// Unified protocol configuration interface - server supports multi-protocol
    pub fn protocol<T: crate::protocol::adapter::DynServerConfig>(mut self, config: T) -> Self {
        let protocol_name = config.protocol_name().to_string();
        self.protocol_configs
            .insert(protocol_name, Box::new(config));
        self
    }

    /// Set buffer size for actor channels.
    ///
    /// The default is whatever `ServerLimits::new().mailbox()` reports — one
    /// value, used whether or not [`Self::limits`] is called (it used to
    /// disagree with `ServerLimits`' own default).
    ///
    /// Prefer [`Self::limits`] with `ServerLimits::mailbox_capacity`; this
    /// remains as a focused shortcut.
    pub fn actor_buffer_size(mut self, size: usize) -> Self {
        self.actor_buffer_size = Some(size);
        self
    }

    /// Apply per-connection resource limits (write deadline, event-pipe and
    /// outbound-queue capacity) plus the per-session actor mailbox capacity.
    pub fn limits(mut self, limits: crate::transport::limits::ServerLimits) -> Self {
        self.transport_config.connection_limits = limits.connection;
        self.actor_buffer_size = Some(limits.mailbox_capacity);
        self
    }

    /// Build the server.
    ///
    /// `handler` is required: each connection gets its own actor that invokes it
    /// for that session's messages and lifecycle. Taking it here rather than via
    /// an optional setter means a server can never be built without a consumer.
    pub async fn build(
        self,
        handler: std::sync::Arc<dyn super::session_actor::SessionHandler>,
    ) -> Result<TransportServer, TransportError> {
        // Validate the cap up front: 0 would mean "reject every connection"
        // (surely a config mistake), and values above Semaphore::MAX_PERMITS
        // would otherwise be silently truncated so logs and behaviour disagree.
        if let Some(max) = self.max_connections {
            if max == 0 {
                return Err(TransportError::config_error(
                    "max_connections",
                    "must be at least 1 (0 would reject every connection)",
                ));
            }
            if max > tokio::sync::Semaphore::MAX_PERMITS {
                return Err(TransportError::config_error(
                    "max_connections",
                    format!(
                        "exceeds the maximum supported value {}",
                        tokio::sync::Semaphore::MAX_PERMITS
                    ),
                ));
            }
        }
        // Validate every protocol config on the ONE construction path (the
        // config-driven builder) — the deleted legacy factory SPI used to be
        // the only place that ran validation, so a zero-stream / oversized
        // config could reach a running server through the builder.
        for (name, config) in &self.protocol_configs {
            config
                .validate_dyn()
                .map_err(|e| TransportError::config_error(name.as_str(), e.to_string()))?;
        }
        let transport_server = super::transport_server::TransportServer::new(
            self.transport_config.clone(),
            self.protocol_configs,
            handler,
            self.actor_buffer_size,
        )
        .await?;
        let mut transport_server = transport_server.with_frame_policy(self.frame_policy);
        if let Some(max) = self.max_connections {
            transport_server = transport_server.with_max_connections(max);
        }

        tracing::info!("[SUCCESS] TransportServer build completed");
        Ok(transport_server)
    }
}

impl Default for TransportServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

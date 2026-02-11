use crate::{
    connection::{Connection, ConnectionFactory, Server},
    error::TransportError,
    protocol::adapter::ConfigError,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Protocol plugin interface
#[async_trait]
pub trait ProtocolPlugin: Send + Sync {
    /// Get protocol name
    fn name(&self) -> &str;

    /// Get protocol description
    fn description(&self) -> &str;

    /// Get protocol features
    fn features(&self) -> Vec<String>;

    /// Get default port
    fn default_port(&self) -> Option<u16>;

    /// Validate configuration
    async fn validate_config(&self, config: &dyn std::any::Any) -> Result<(), ConfigError>;

    /// Create server
    async fn create_server(
        &self,
        config: Box<dyn std::any::Any>,
    ) -> Result<Box<dyn Server>, TransportError>;

    /// Create connection factory
    async fn create_connection_factory(
        &self,
        config: Box<dyn std::any::Any>,
    ) -> Result<Box<dyn ConnectionFactory>, TransportError>;

    /// Supported configuration type name
    fn config_type_name(&self) -> &str;
}

/// Plugin registry
pub struct PluginRegistry {
    plugins: Arc<RwLock<HashMap<String, Arc<dyn ProtocolPlugin>>>>,
}

impl PluginRegistry {
    /// Create new plugin registry
    pub fn new() -> Self {
        Self {
            plugins: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register plugin
    pub fn register(&self, plugin: Arc<dyn ProtocolPlugin>) -> Result<(), TransportError> {
        let mut plugins = self.plugins.write().unwrap();
        let name = plugin.name().to_string();

        if plugins.contains_key(&name) {
            return Err(TransportError::config_error(
                "plugin",
                format!("Protocol '{}' is already registered", name),
            ));
        }

        plugins.insert(name, plugin);
        Ok(())
    }

    /// Unregister plugin
    pub fn unregister(&self, name: &str) -> Result<(), TransportError> {
        let mut plugins = self.plugins.write().unwrap();
        if plugins.remove(name).is_some() {
            Ok(())
        } else {
            Err(TransportError::config_error(
                "plugin",
                format!("Protocol '{}' is not registered", name),
            ))
        }
    }

    /// Get plugin
    pub fn get(&self, name: &str) -> Option<Arc<dyn ProtocolPlugin>> {
        let plugins = self.plugins.read().unwrap();
        plugins.get(name).cloned()
    }

    /// List all plugins
    pub fn list(&self) -> Vec<String> {
        let plugins = self.plugins.read().unwrap();
        plugins.keys().cloned().collect()
    }

    /// Get plugin information
    pub fn get_plugin_info(&self, name: &str) -> Option<PluginInfo> {
        let plugins = self.plugins.read().unwrap();
        plugins.get(name).map(|plugin| PluginInfo {
            name: plugin.name().to_string(),
            description: plugin.description().to_string(),
            features: plugin.features(),
            default_port: plugin.default_port(),
            config_type: plugin.config_type_name().to_string(),
        })
    }

    /// Get all plugin information
    pub fn get_all_plugin_info(&self) -> Vec<PluginInfo> {
        let plugins = self.plugins.read().unwrap();
        plugins
            .values()
            .map(|plugin| PluginInfo {
                name: plugin.name().to_string(),
                description: plugin.description().to_string(),
                features: plugin.features(),
                default_port: plugin.default_port(),
                config_type: plugin.config_type_name().to_string(),
            })
            .collect()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Plugin information
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// Plugin name
    pub name: String,
    /// Plugin description
    pub description: String,
    /// Plugin features
    pub features: Vec<String>,
    /// Default port
    pub default_port: Option<u16>,
    /// Configuration type name
    pub config_type: String,
}

/// Plugin manager
pub struct PluginManager {
    registry: PluginRegistry,
    /// Whether to allow overriding built-in protocols
    allow_override: bool,
}

impl PluginManager {
    /// Create new plugin manager
    pub fn new() -> Self {
        Self {
            registry: PluginRegistry::new(),
            allow_override: false,
        }
    }

    /// Create plugin manager with override allowed
    pub fn with_override() -> Self {
        Self {
            registry: PluginRegistry::new(),
            allow_override: true,
        }
    }

    /// Register plugin
    pub fn register_plugin(&self, plugin: Arc<dyn ProtocolPlugin>) -> Result<(), TransportError> {
        let name = plugin.name();

        // Check if it's a builtin protocol
        if !self.allow_override && self.is_builtin_protocol(name) {
            return Err(TransportError::config_error(
                "plugin",
                format!(
                    "Cannot override builtin protocol '{}'. Use with_override() to allow this.",
                    name
                ),
            ));
        }

        self.registry.register(plugin)
    }

    /// Unregister plugin
    pub fn unregister_plugin(&self, name: &str) -> Result<(), TransportError> {
        self.registry.unregister(name)
    }

    /// Get plugin
    pub fn get_plugin(&self, name: &str) -> Option<Arc<dyn ProtocolPlugin>> {
        self.registry.get(name)
    }

    /// List all protocols (including builtin and plugins)
    pub fn list_protocols(&self) -> Vec<String> {
        let mut protocols = vec![
            "tcp".to_string(),
            "websocket".to_string(),
            "quic".to_string(),
        ];
        protocols.extend(self.registry.list());
        protocols.sort();
        protocols.dedup();
        protocols
    }

    /// Get protocol information
    pub fn get_protocol_info(&self, name: &str) -> Option<PluginInfo> {
        // Check plugins first
        if let Some(info) = self.registry.get_plugin_info(name) {
            return Some(info);
        }

        // Then check builtin protocols
        match name {
            "tcp" => Some(PluginInfo {
                name: "tcp".to_string(),
                description:
                    "Transmission Control Protocol - reliable, ordered, connection-oriented"
                        .to_string(),
                features: vec![
                    "reliable".to_string(),
                    "ordered".to_string(),
                    "connection-oriented".to_string(),
                ],
                default_port: Some(8080),
                config_type: "TcpConfig".to_string(),
            }),
            "websocket" => Some(PluginInfo {
                name: "websocket".to_string(),
                description: "WebSocket Protocol - full-duplex communication over HTTP".to_string(),
                features: vec![
                    "full-duplex".to_string(),
                    "http-upgrade".to_string(),
                    "frame-based".to_string(),
                ],
                default_port: Some(8080),
                config_type: "WebSocketConfig".to_string(),
            }),
            "quic" => Some(PluginInfo {
                name: "quic".to_string(),
                description: "QUIC Protocol - modern transport with built-in encryption"
                    .to_string(),
                features: vec![
                    "encrypted".to_string(),
                    "multiplexed".to_string(),
                    "low-latency".to_string(),
                ],
                default_port: Some(4433),
                config_type: "QuicConfig".to_string(),
            }),
            _ => None,
        }
    }

    /// Check if it's a builtin protocol
    fn is_builtin_protocol(&self, name: &str) -> bool {
        matches!(name, "tcp" | "websocket" | "quic")
    }

    /// Create server with plugin
    pub async fn create_server_with_plugin(
        &self,
        protocol: &str,
        config: Box<dyn std::any::Any>,
    ) -> Result<Box<dyn Server>, TransportError> {
        if let Some(plugin) = self.get_plugin(protocol) {
            plugin.create_server(config).await
        } else {
            Err(TransportError::config_error(
                "plugin",
                format!(
                    "Protocol '{}' not found. Available protocols: {:?}",
                    protocol,
                    self.list_protocols()
                ),
            ))
        }
    }

    /// Create connection factory with plugin
    pub async fn create_connection_factory_with_plugin(
        &self,
        protocol: &str,
        config: Box<dyn std::any::Any>,
    ) -> Result<Box<dyn ConnectionFactory>, TransportError> {
        if let Some(plugin) = self.get_plugin(protocol) {
            plugin.create_connection_factory(config).await
        } else {
            Err(TransportError::config_error(
                "plugin",
                format!(
                    "Protocol '{}' not found. Available protocols: {:?}",
                    protocol,
                    self.list_protocols()
                ),
            ))
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Connection wrapper for plugins
pub struct PluginConnection {
    inner: Box<dyn Connection>,
}

impl PluginConnection {
    pub fn new(inner: Box<dyn Connection>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Connection for PluginConnection {
    async fn send(&mut self, packet: crate::packet::Packet) -> Result<(), TransportError> {
        self.inner.send(packet).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await
    }

    fn session_id(&self) -> crate::SessionId {
        self.inner.session_id()
    }

    fn set_session_id(&mut self, session_id: crate::SessionId) {
        self.inner.set_session_id(session_id)
    }

    fn connection_info(&self) -> crate::command::ConnectionInfo {
        self.inner.connection_info()
    }

    fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    async fn flush(&mut self) -> Result<(), TransportError> {
        self.inner.flush().await
    }

    fn event_stream(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::event::TransportEvent>> {
        self.inner.event_stream()
    }
}

use crate::{
    connection::{Connection, Server}, // Use unified interface
    error::TransportError,

    SessionId,
};
/// Protocol abstraction layer
///
/// Provides modular protocol support, allowing users to register custom protocols
use async_trait::async_trait;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Future type alias, simplifying type signatures
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Protocol factory trait
///
/// Each protocol module needs to implement this trait to create connections and servers
#[async_trait]
pub trait ProtocolFactory: Send + Sync {
    /// Protocol name (e.g. "tcp", "websocket", "quic")
    fn protocol_name(&self) -> &'static str;

    /// Supported URL schemes (e.g. ["tcp", "tcp4", "tcp6"])
    fn supported_schemes(&self) -> Vec<&'static str>;

    /// Create client connection
    async fn create_connection(
        &self,
        uri: &str,
        config: Option<Box<dyn std::any::Any + Send + Sync>>,
    ) -> Result<Box<dyn Connection>, TransportError>;

    /// Create server
    async fn create_server(
        &self,
        bind_addr: &str,
        config: Option<Box<dyn std::any::Any + Send + Sync>>,
    ) -> Result<Box<dyn Server>, TransportError>;

    /// Parse URI to connection parameters
    fn parse_uri(
        &self,
        uri: &str,
    ) -> Result<(std::net::SocketAddr, HashMap<String, String>), TransportError> {
        // Default implementation: simple host:port parsing
        if let Ok(addr) = uri.parse::<std::net::SocketAddr>() {
            Ok((addr, HashMap::new()))
        } else {
            Err(TransportError::config_error(
                "uri",
                format!("Invalid URI: {}", uri),
            ))
        }
    }

    /// 获取默认配置
    fn default_config(&self) -> Box<dyn std::any::Any + Send + Sync>;
}

/// 协议注册表
///
/// 管理所有可用的协议工厂
pub struct ProtocolRegistry {
    /// 按协议名称索引的工厂
    factories: RwLock<HashMap<String, Arc<dyn ProtocolFactory>>>,
    /// 按URL scheme索引的工厂  
    schemes: RwLock<HashMap<String, Arc<dyn ProtocolFactory>>>,
}

impl ProtocolRegistry {
    /// 创建新的协议注册表
    pub fn new() -> Self {
        Self {
            factories: RwLock::new(HashMap::new()),
            schemes: RwLock::new(HashMap::new()),
        }
    }

    /// 注册协议工厂
    pub async fn register<F>(&self, factory: F) -> Result<(), TransportError>
    where
        F: ProtocolFactory + 'static,
    {
        let factory = Arc::new(factory);
        let protocol_name = factory.protocol_name().to_string();
        let schemes = factory.supported_schemes();

        // 注册到协议名称映射
        {
            let mut factories = self.factories.write().await;
            if factories.contains_key(&protocol_name) {
                return Err(TransportError::config_error(
                    "protocol_name",
                    format!("Protocol '{}' already registered", protocol_name),
                ));
            }
            factories.insert(protocol_name.clone(), factory.clone());
        }

        // 注册到scheme映射
        {
            let mut schemes_map = self.schemes.write().await;
            for scheme in schemes {
                if schemes_map.contains_key(scheme) {
                    return Err(TransportError::config_error(
                        "scheme",
                        format!("Scheme '{}' already registered", scheme),
                    ));
                }
                schemes_map.insert(scheme.to_string(), factory.clone());
            }
        }

        tracing::info!("Registered protocol: {}", protocol_name);
        Ok(())
    }

    /// 根据协议名称获取工厂
    pub async fn get_factory(&self, protocol: &str) -> Option<Arc<dyn ProtocolFactory>> {
        self.factories.read().await.get(protocol).cloned()
    }

    /// 根据URL scheme获取工厂
    pub async fn get_factory_by_scheme(&self, scheme: &str) -> Option<Arc<dyn ProtocolFactory>> {
        self.schemes.read().await.get(scheme).cloned()
    }

    /// 创建连接
    pub async fn create_connection(
        &self,
        uri: &str,
        config: Option<Box<dyn std::any::Any + Send + Sync>>,
    ) -> Result<Box<dyn Connection>, TransportError> {
        let scheme = self.extract_scheme(uri)?;

        if let Some(factory) = self.get_factory_by_scheme(&scheme).await {
            factory.create_connection(uri, config).await
        } else {
            Err(TransportError::protocol_error(
                "unknown",
                format!("Unsupported protocol scheme: {}", scheme),
            ))
        }
    }

    /// 创建服务器
    pub async fn create_server(
        &self,
        bind_addr: &str,
        protocol: &str,
        config: Option<Box<dyn std::any::Any + Send + Sync>>,
    ) -> Result<Box<dyn Server>, TransportError> {
        if let Some(factory) = self.get_factory(protocol).await {
            factory.create_server(bind_addr, config).await
        } else {
            Err(TransportError::protocol_error(
                "unknown",
                format!("Unsupported protocol: {}", protocol),
            ))
        }
    }

    /// 列出所有已注册的协议
    pub async fn list_protocols(&self) -> Vec<String> {
        self.factories.read().await.keys().cloned().collect()
    }

    /// 列出所有支持的URL schemes
    pub async fn list_schemes(&self) -> Vec<String> {
        self.schemes.read().await.keys().cloned().collect()
    }

    /// 从URI中提取scheme
    fn extract_scheme(&self, uri: &str) -> Result<String, TransportError> {
        if let Some(pos) = uri.find("://") {
            Ok(uri[..pos].to_string())
        } else {
            // 如果没有scheme，尝试判断是否是简单的 host:port 格式
            if uri.parse::<std::net::SocketAddr>().is_ok() {
                Ok("tcp".to_string()) // 默认为TCP
            } else {
                Err(TransportError::config_error(
                    "uri",
                    format!("Invalid URI format: {}", uri),
                ))
            }
        }
    }
}

impl Default for ProtocolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 预留的扩展点：类型级协议集合
///
/// 这将在第二阶段实现，提供零开销的编译时协议支持
pub trait ProtocolSet: Send + Sync {
    /// 创建连接（编译时分发）
    fn create_connection(&self, uri: &str) -> BoxFuture<Result<SessionId, TransportError>>;

    /// 创建服务器（编译时分发）
    fn create_server(&self, bind_addr: &str) -> BoxFuture<Result<Box<dyn Server>, TransportError>>;
}

/// 预留的标准协议集合
///
/// 这将在第二阶段实现，提供零开销的默认协议支持
#[allow(dead_code)]
pub struct StandardProtocols {
    // TODO: 第二阶段实现
    _phantom: std::marker::PhantomData<()>,
}

/// 预留的插件管理器
///
/// 这将在第三阶段实现，支持动态库加载
#[allow(dead_code)]
pub struct PluginManager {
    registry: ProtocolRegistry,
    // TODO: 第三阶段添加动态库加载功能
}

#[allow(dead_code)]
impl PluginManager {
    /// 预留方法：从动态库加载协议
    pub fn load_from_dylib(&mut self, _path: &std::path::Path) -> Result<(), TransportError> {
        // TODO: 第三阶段实现
        Err(TransportError::config_error(
            "plugin",
            "Plugin loading not yet implemented",
        ))
    }
}

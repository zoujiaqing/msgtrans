# 🚀 MsgTrans - Marco de Comunicación Multi-Protocolo Moderno

[![Rust](https://img.shields.io/badge/rust-1.80+-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache-blue.svg)](https://github.com/zoujiaqing/msgtrans/blob/main/LICENSE)
[![Crates.io](https://img.shields.io/crates/v/msgtrans.svg)](https://crates.io/crates/msgtrans)
[![Docs.rs](https://img.shields.io/docsrs/msgtrans)](https://docs.rs/msgtrans)

🌐 Idioma: [Inglés](README.md) | [简体中文](README.zh-CN.md)

> **Marco de comunicación multi-protocolo moderno con una interfaz unificada sobre TCP, WebSocket y QUIC**

## 🌟 Características Principales

### 🏗️ Arquitectura Unificada

- **Arquitectura de tres capas**: Aplicación → Transporte → Protocolo, con separación clara
- **Lógica de negocio agnóstica al protocolo**: un código base, despliegue multi-protocolo
- **Configuración impulsada**: cambie protocolos mediante configuración sin cambiar la lógica de negocio
- **Adaptadores conectables**: implemente el trait `Connection` para añadir un nuevo protocolo

### ⚡ Concurrencia Moderna

- **Internos sin bloqueos**: actores por sesión y mapas sin bloqueos evitan la contención de Mutex en la ruta crítica
- **Paquetes sin copia**: `Packet` lleva una carga útil `Bytes`, entregada directamente al cable cuando es posible
- **Modelo basado en eventos**: completamente asíncrono, manejo de eventos no bloqueante
- **Presión de salida limitada**: las colas de sal están limitadas por conexión, por lo que un par lento no agota memoria ni bloquea un bucle de difusión

### 🔌 Protocolos

- **TCP** - transporte confiable de flujos
- **WebSocket** - comunicación web en tiempo real
- **QUIC** - transporte moderno basado en UDP
- **Protocolos personalizados** - implemente el trait `Connection`

### 🎯 API Minimalista

- **Patrón constructor**: configuración fluida y legible
- **Seguridad de tipos**: configuración verificada en tiempo de compilación
- **Valores predeterminados sensatos**: funciona listo para usar, ajuste solo cuando sea necesario

## 🚀 Inicio Rápido

### Instalación

```toml
[dependencies]
msgtrans = "1.0"
```

### Crear un Servidor Multi-Protocolo

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    Responder, SessionHandler, SessionSender, TransportServerBuilder,
    TcpServerConfig, WebSocketServerConfig, QuicServerConfig,
    Packet,
    SessionId,
};
use std::sync::Arc;

// La lógica de negocio vive en un manejador. Cada conexión obtiene su propio actor que
// lo llama, por lo que un manejador lento ralentiza solo su propia conexión en lugar de
// descartar mensajes para todos.
struct Echo;

#[async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, _session: SessionId, packet: Packet, sender: SessionSender) {
        // Tráfico unidireccional: devuélvelo - transparente para el protocolo.
        let response = format!("Echo: {}", String::from_utf8_lossy(packet.payload()));
        let _ = sender.send_data(response.into_bytes()).await;
    }

    async fn on_request(&self, _session: SessionId, request: Packet, responder: Responder) {
        // Las solicitudes conllevan la obligación de responder: el Responder consumidor
        // es la única forma de hacerlo, y Ok(Written) significa que los bytes se escribieron.
        let _ = responder.respond(request.into_payload()).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure múltiples protocolos - la misma lógica de negocio sirve para todos ellos.
    let tcp_config = TcpServerConfig::new("127.0.0.1:8001")?;
    let websocket_config = WebSocketServerConfig::new("127.0.0.1:8002")?.path("/ws");
    let quic_config = QuicServerConfig::new("127.0.0.1:8003")?;

    let server = TransportServerBuilder::new()
        .max_connections(10000)
        .protocol(tcp_config)
        .protocol(websocket_config)
        .protocol(quic_config)
        .build(Arc::new(Echo))
        .await?;

    // Se ejecuta hasta que el servidor se detenga.
    server.serve().await?;
    Ok(())
}
```

### Crear una Conexión de Cliente

```rust,no_run
use msgtrans::{
    TransportClientBuilder,
    TcpClientConfig,
    ClientEvent,
};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tcp_config = TcpClientConfig::new("127.0.0.1:8001")?
        .connect_timeout(Duration::from_secs(30));

    let mut client = TransportClientBuilder::new()
        .protocol(tcp_config)
        .build()
        .await?;

    client.connect().await?;

    // Envía un mensaje unidireccional. `send` es una confirmación de escritura: devuelve una vez que
    // los bytes llegaron al socket (use `send_detached` para envío e olvido).
    let receipt = client.send("Hello, MsgTrans!".as_bytes()).await?;
    println!("Message sent (id {})", receipt.message_id);

    // Envía una solicitud y espera la respuesta. `Ok` lleva los bytes de respuesta;
    // cada fallo, incluyendo un tiempo de espera agotado, es un `Err`.
    let response = client.request("What time is it?".as_bytes()).await?;
    println!("Received response: {}", String::from_utf8_lossy(&response));

    // Consume eventos. El flujo tiene un solo consumidor y se toma una vez.
    let mut events = client.events().await?;
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            match event {
                ClientEvent::Message(msg) => {
                    println!("Received: {}", msg.as_text_lossy());
                }
                ClientEvent::Request(req) => {
                    // Solicitud iniciada por el servidor: `req` es consumidor; respóndale.
                    req.respond_detached(b"ack".to_vec());
                }
                ClientEvent::Disconnected { .. } => break,
                _ => {}
            }
        }
    });

    Ok(())
}
```

## 🏗️ Diseño de Arquitectura

### Arquitectura de Tres Capas

```text
+-------------------------------------+
|  Application Layer                  |  <- Business logic, protocol-agnostic
+-------------------------------------+
|  Transport Layer                    |  <- Connection management, unified API
|  - TransportServer / TransportClient|     - Connection lifecycle
|  - SessionActor                     |     - Event routing
|  - RequestRegistry                  |     - Request/response lifecycle
+-------------------------------------+
|  Protocol Layer                     |  <- Protocol implementation
|  - TCP / WebSocket / QUIC adapters  |     - Connection trait
|  - Protocol configs                 |     - Protocol registration
+-------------------------------------+
```

### Principios de Diseño

**Abstracción unificada, transparencia de protocolo** — `TransportServer`/`TransportClient`
expongen una sola interfaz comercial; cada adaptador implementa el trait `Connection` y
oculta los detalles del protocolo.

**Impulsado por configuración** — el mismo código de servidor funciona en cualquier protocolo; solo la
configuración pasada a `.protocol(..)` cambia:

```rust,no_run
# use msgtrans::{TransportServerBuilder, SessionHandler, SessionSender, TcpServerConfig, QuicServerConfig, Packet, SessionId};
# use std::sync::Arc;
# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
#     async fn on_request(&self, _s: SessionId, _p: Packet, _r: msgtrans::Responder) {}
# }
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
# let handler = Arc::new(H);
// TCP server
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("0.0.0.0:8080")?)
    .build(handler.clone()).await?;

// QUIC server - identical business logic
let server = TransportServerBuilder::new()
    .protocol(QuicServerConfig::new("0.0.0.0:8080")?)
    .build(handler).await?;
# Ok(()) }
```

### Modelo de Manejador

El servidor entrega el tráfico de cada sesión a un `SessionHandler`; el cliente
aún expone un flujo de eventos, ya que un cliente tiene exactamente una conexión.

```rust
use msgtrans::{
    ClientEvent,
    ConnectionInfo,
    TransportError, CloseReason,
    Packet,
    Responder, SessionHandler, SessionSender,
    SessionId,
};

// Lado del servidor: implementar el manejador. `on_message` (tráfico unidireccional) y
// `on_request` (solicitudes, respondidas a través del Responder consumidor) son
// obligatorios; los ganchos del ciclo de vida son opcionales.
struct MyHandler;

#[async_trait::async_trait]
impl SessionHandler for MyHandler {
    async fn on_message(&self, session_id: SessionId, packet: Packet, sender: SessionSender) { /* ... */ }
    async fn on_request(&self, session_id: SessionId, request: Packet, responder: Responder) {
        let _ = responder.respond(request.into_payload()).await; // Ok(Written) == bytes written
    }
    async fn on_connected(&self, session_id: SessionId, info: ConnectionInfo) { /* ... */ }
    async fn on_disconnected(&self, session_id: SessionId, reason: CloseReason) { /* ... */ }
    async fn on_error(&self, session_id: SessionId, error: TransportError) { /* ... */ }
}

// Client side: events
# fn _client_events(ev: ClientEvent) { match ev {
ClientEvent::Connected { info } => { /* ... */ }
ClientEvent::Message(msg) => { /* one-way data; msg.payload() */ }
ClientEvent::Request(req) => { /* consuming; req.respond_detached(..) */ }
ClientEvent::MessageSent { message_id } => { /* ... */ }
ClientEvent::Disconnected { reason } => { /* ... */ }
ClientEvent::Error { error } => { /* ... */ }
# _ => {} } }
```

## ⚡ Patrones de Uso

### Envío Concurrente

`TransportServer` es fácilmente clonable (comparte estado a través de `Arc`), por lo que puede ser
movido a tareas generadas para acceso concurrente y sin bloqueos a sesiones:

```rust,no_run
# use msgtrans::{TransportServer, SessionHandler, SessionSender, Packet, SessionId};
# use std::sync::Arc;
struct Echo { server: TransportServer }

#[async_trait::async_trait]
impl SessionHandler for Echo {
    async fn on_message(&self, session_id: SessionId, packet: Packet, _tx: SessionSender) {
        // Delegar el trabajo a una tarea generada mantiene el actor de esta sesión libre para
        // recoger el siguiente mensaje.
        let server = self.server.clone();
        tokio::spawn(async move {
            let response = format!("Echo: {}", String::from_utf8_lossy(packet.payload()));
            let _ = server.send(session_id, response.as_bytes()).await;
        });
    }

    async fn on_request(&self, _s: SessionId, request: Packet, responder: msgtrans::Responder) {
        // respond_detached entrega la escritura; el registro aún registra
        // el resultado real.
        responder.respond_detached(request.into_payload());
    }
}
```

### Solicitud / Respuesta

```rust,no_run
# use msgtrans::TransportClient;
# async fn f(client: &TransportClient) -> Result<(), Box<dyn std::error::Error>> {
// `Ok` lleva los bytes de respuesta; un tiempo de espera es un `Err`.
let response = client.request(b"Get user data").await?;
println!("Got {} bytes", response.len());
# Ok(()) }
```

## 🔌 Extensión de Protocolo

Para añadir un protocolo, implemente el trait `Connection` para su adaptador y un
tipo de configuración correspondiente. Consulte los adaptadores integrados `adapters::{tcp, websocket, quic}` para
referencias completas y funcionales; el esquema siguiente muestra la estructura:

```rust
use msgtrans::spi::{
    event_channel, CloseReason, Connection, ConnectionEvents, ConnectionInfo, ConnectionWriter,
    EventSink, Packet, SessionId, TransportError, WriteCompletion,
};
use msgtrans::FramePolicy;
use std::sync::Arc;

/// La mitad de escritura. Manténla separada de la conexión para que el transporte pueda clonarla
/// bajo su bloqueo de conexión y LIBERAR el bloqueo antes de esperar la
/// encolación — una cola saturada no debe detener reconectar/cerrar/detener.
struct MyWriter {
    queue: tokio::sync::mpsc::Sender<(Packet, WriteCompletion)>,
}

#[async_trait::async_trait]
impl ConnectionWriter for MyWriter {
    async fn send_with_completion(
        &self,
        packet: Packet,
        completion: WriteCompletion,
    ) -> Result<(), TransportError> {
        // Entrega TANTO el paquete como su confirmación al bucle de escritura. La
        // confirmación solo debe resolverse una vez que los bytes hayan llegado realmente al
        // socket — resolverla aquí sería una confirmación de escritura falsa, y el
        // punto de `WriteCompletion` es que `Ok` sea demostrable.
        //
        // Si la cola ha desaparecido, DESCARTA la confirmación: al descartarla se reporta fallo
        // por construcción, por lo que no hay camino que invente un `Ok`.
        self.queue
            .send((packet, completion))
            .await
            .map_err(|_| TransportError::connection_error("connection closed", false))
    }
}

/// El bucle de escritura que posee el socket. Es lo único autorizado a decir que una
/// escritura tuvo éxito.
async fn write_loop(mut queue: tokio::sync::mpsc::Receiver<(Packet, WriteCompletion)>) {
    while let Some((packet, completion)) = queue.recv().await {
        let bytes = match packet.try_encode() {
            Ok(bytes) => bytes,
            Err(e) => {
                completion.complete(Err(TransportError::protocol_error("encode", e.to_string())));
                continue;
            }
        };
        // Reemplaza con tu escritura real de socket; reporta exactamente lo que devolvió.
        let written: Result<(), TransportError> = my_socket_write(&bytes).await;
        completion.complete(written);
    }
}
# async fn my_socket_write(_bytes: &[u8]) -> Result<(), TransportError> { Ok(()) }

pub struct MyAdapter {
    session_id: SessionId,
    sink: EventSink,
    events: Option<ConnectionEvents>,
    queue: tokio::sync::mpsc::Sender<(Packet, WriteCompletion)>,
}

impl MyAdapter {
    pub fn new() -> Self {
        let (sink, events) = event_channel(std::num::NonZeroUsize::new(1024).unwrap());
        let (queue, rx) = tokio::sync::mpsc::channel(1024);
        tokio::spawn(write_loop(rx));
        Self { session_id: SessionId::new(0), sink, events: Some(events), queue }
    }

    /// Tu bucle de lectura empuja paquetes entrantes aquí. La tubería descomprime y
    /// normaliza, por lo que cada consumidor ve texto sin formato.
    pub async fn on_packet(&self, packet: Packet) -> bool {
        self.sink.message(packet).await
    }
}

#[async_trait::async_trait]
impl Connection for MyAdapter {
    fn writer(&self) -> Arc<dyn ConnectionWriter> {
        Arc::new(MyWriter { queue: self.queue.clone() })
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        self.sink.close(CloseReason::Normal);
        Ok(())
    }
    fn session_id(&self) -> SessionId { self.session_id }
    fn set_session_id(&mut self, session_id: SessionId) { self.session_id = session_id; }
    fn connection_info(&self) -> ConnectionInfo { ConnectionInfo::default() }
    fn is_connected(&self) -> bool { true }
    async fn flush(&mut self) -> Result<(), TransportError> { Ok(()) }
    fn take_event_pipe(&mut self) -> Option<ConnectionEvents> { self.events.take() }
    fn set_frame_policy(&self, _policy: FramePolicy) { /* honor Strict here */ }
}
```

## 📖 Ejemplos de Uso

### Servidor WebSocket

```rust,no_run
use async_trait::async_trait;
use msgtrans::{
    Responder, SessionHandler, SessionSender, TransportServerBuilder,
    WebSocketServerConfig,
    Packet,
    SessionId,
};
use std::sync::Arc;

struct Chat;

#[async_trait]
impl SessionHandler for Chat {
    async fn on_message(&self, _session: SessionId, packet: Packet, sender: SessionSender) {
        let msg = String::from_utf8_lossy(packet.payload());
        let _ = sender.send_data(format!("You said: {msg}").into_bytes()).await;
    }

    async fn on_request(&self, _session: SessionId, request: Packet, responder: Responder) {
        let msg = String::from_utf8_lossy(request.payload()).to_string();
        let _ = responder.respond(format!("You asked: {msg}").into_bytes()).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = WebSocketServerConfig::new("127.0.0.1:8080")?.path("/chat");

    let server = TransportServerBuilder::new()
        .protocol(config)
        .max_connections(1000)
        .build(Arc::new(Chat))
        .await?;

    server.serve().await?;
    Ok(())
}
```

### Cliente QUIC

```rust,no_run
use msgtrans::{
    TransportClientBuilder,
    QuicClientConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Servidor de prueba local / autofirmado: omitir la verificación de certificados.
    // En producción, elimina danger_skip_verification() y configura un real
    // nombre de servidor y CA en su lugar.
    let config = QuicClientConfig::new("127.0.0.1:8003")?
        .server_name("localhost")
        .danger_skip_verification();

    let mut client = TransportClientBuilder::new()
        .protocol(config)
        .build()
        .await?;

    client.connect().await?;

    for i in 0..1000u32 {
        client.send(format!("message {i}").as_bytes()).await?;
    }
    println!("Done");
    Ok(())
}
```

## 🛠️ Opciones de Configuración

### Configuración del Servidor

```rust,no_run
use msgtrans::{TcpServerConfig, WebSocketServerConfig, QuicServerConfig};
use std::time::Duration;

# fn f() -> Result<(), Box<dyn std::error::Error>> {
let tcp_config = TcpServerConfig::new("0.0.0.0:8001")?
    .keepalive(Some(Duration::from_secs(60)))
    .nodelay(true)
    .reuse_addr(true);

let ws_config = WebSocketServerConfig::new("0.0.0.0:8002")?
    .path("/api/ws")
    .max_frame_size(1024 * 1024);

let quic_config = QuicServerConfig::new("0.0.0.0:8003")?
    .cert_pem(std::fs::read_to_string("cert.pem")?)
    .key_pem(std::fs::read_to_string("key.pem")?)
    .max_concurrent_streams(1000);
# Ok(()) }
```

## 🔧 Características Avanzadas

### Estadísticas

```rust,no_run
# use msgtrans::TransportServer;
# async fn f(server: &TransportServer) {
let active = server.session_count().await;
println!("Active sessions: {active}");
# }
```

### Manejo Elegante de Errores

```rust,no_run
# use msgtrans::{TransportClient, TransportError};
# async fn f(client: &mut TransportClient) -> Result<(), Box<dyn std::error::Error>> {
match client.send("Hello, World!".as_bytes()).await {
    Ok(result) => println!("Sent (ID: {})", result.message_id),
    Err(TransportError::Connection { .. }) => {
        println!("Connection lost, reconnecting");
        client.connect().await?;
    }
    Err(TransportError::Protocol { protocol, reason }) => {
        println!("Protocol error [{protocol}]: {reason}");
    }
    Err(e) => println!("Other error: {e}"),
}
# Ok(()) }
```

### Cierre Elegante

```rust,no_run
use msgtrans::{TransportServerBuilder, SessionHandler, SessionSender, TcpServerConfig, Packet, SessionId};
use std::sync::Arc;

# struct H;
# #[async_trait::async_trait]
# impl SessionHandler for H {
#     async fn on_message(&self, _s: SessionId, _p: Packet, _tx: SessionSender) {}
#     async fn on_request(&self, _s: SessionId, _p: Packet, _r: msgtrans::Responder) {}
# }
# async fn f() -> Result<(), Box<dyn std::error::Error>> {
let server = TransportServerBuilder::new()
    .protocol(TcpServerConfig::new("0.0.0.0:8001")?)
    .build(Arc::new(H)).await?;

// ... later, drain active sessions (TransportConfig::graceful_timeout):
server.stop().await;
# Ok(()) }
```

## 📚 Documentación y Ejemplos

El directorio [`examples/`](examples/) contiene programas completos y ejecutables:

- [`echo_server.rs`](examples/echo_server.rs) - servidor de eco multi-protocolo
- [`echo_client_tcp.rs`](examples/echo_client_tcp.rs) - cliente TCP
- [`echo_client_websocket.rs`](examples/echo_client_websocket.rs) - cliente WebSocket
- [`echo_client_quic.rs`](examples/echo_client_quic.rs) - cliente QUIC
- [`load_test.rs`](examples/load_test.rs) / [`load_test_server.rs`](examples/load_test_server.rs) - prueba de carga
- [`packet.rs`](examples/packet.rs) - serialización de paquetes

```bash
# Start the multi-protocol echo server
cargo run --example echo_server

# In another terminal, run a client
cargo run --example echo_client_tcp
```

## 🏆 Casos de Uso

- **Game servers** - comunicación en tiempo real de alta concurrencia
- **Chat systems** - mensajería instantánea multi-protocolo
- **Microservice communication** - transporte eficiente entre servicios
- **Real-time data** - sistemas financieros, de monitoreo y telemetría
- **IoT platforms** - gestión de conexiones de dispositivos a gran escala
- **Protocol gateways** - conversión y proxy multi-protocolo

## 📝 Licencia

Licenciado bajo la [Apache License 2.0](https://github.com/zoujiaqing/msgtrans/blob/main/LICENSE).

Copyright © 2024 [zoujiaqing](mailto:zoujiaqing@gmail.com)

## 🤝 Contribuyendo

¡Son bienvenidas las Issues y Pull Requests!

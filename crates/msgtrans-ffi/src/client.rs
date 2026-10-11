use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};

use crate::config::{self, ClientConfig};
use crate::error::MsgtransError;
use crate::events::{ClientEvent, map_event};
use crate::runtime::{on_runtime, run_immediate};

#[derive(Debug, Clone, uniffi::Record)]
pub struct SendReceipt {
    pub message_id: u32,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct SendOptions {
    pub biz_type: Option<u8>,
    pub ext_header: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct RequestOptions {
    pub biz_type: Option<u8>,
    pub ext_header: Option<Vec<u8>>,
    /// Overrides the default request timeout.
    pub timeout_ms: Option<u64>,
}

/// A msgtrans client: one connection over TCP, WebSocket or QUIC.
///
/// All methods are safe to call concurrently. `connect` and `shutdown` take
/// exclusive access to the connection, so they wait for in-flight sends and
/// requests to finish first.
#[derive(uniffi::Object)]
pub struct MsgtransClient {
    client: RwLock<msgtrans::TransportClient>,
    // msgtrans's event stream has a single consumer; it is taken once here and
    // shared by every `next_event` caller in arrival order.
    events: Mutex<msgtrans::ClientEvents>,
}

#[uniffi::export]
impl MsgtransClient {
    /// Build a client. Does no I/O; call `connect` to open the connection.
    #[uniffi::constructor]
    pub fn new(config: ClientConfig) -> Result<Arc<Self>, MsgtransError> {
        let builder = config::builder(&config)?;
        let client = run_immediate(builder.build())?;
        let events = run_immediate(client.events())?;
        Ok(Arc::new(Self {
            client: RwLock::new(client),
            events: Mutex::new(events),
        }))
    }

    pub async fn connect(&self) -> Result<(), MsgtransError> {
        on_runtime(async { self.client.write().await.connect().await }).await?;
        Ok(())
    }

    /// Close the connection. The client can `connect` again afterwards.
    pub async fn disconnect(&self) -> Result<(), MsgtransError> {
        on_runtime(async { self.client.read().await.disconnect().await }).await?;
        Ok(())
    }

    /// Close the connection and stop the client. `next_event` returns `null`
    /// once the remaining events have been drained.
    pub async fn shutdown(&self) -> Result<(), MsgtransError> {
        on_runtime(async { self.client.write().await.shutdown().await }).await?;
        Ok(())
    }

    pub async fn is_connected(&self) -> bool {
        on_runtime(async { self.client.read().await.is_connected().await }).await
    }

    /// Send a one-way message. Returns once it was written to the socket.
    pub async fn send(&self, payload: Vec<u8>) -> Result<SendReceipt, MsgtransError> {
        let receipt = on_runtime(async { self.client.read().await.send(&payload).await }).await?;
        Ok(SendReceipt {
            message_id: receipt.message_id,
        })
    }

    /// Send a one-way message. Returns once it was queued, not written.
    pub async fn send_detached(&self, payload: Vec<u8>) -> Result<SendReceipt, MsgtransError> {
        let receipt =
            on_runtime(async { self.client.read().await.send_detached(&payload).await }).await?;
        Ok(SendReceipt {
            message_id: receipt.message_id,
        })
    }

    /// Send a one-way message with a business type and/or extension header.
    /// Returns once it was written to the socket.
    pub async fn send_with_options(
        &self,
        payload: Vec<u8>,
        options: SendOptions,
    ) -> Result<(), MsgtransError> {
        let options = send_options(options);
        on_runtime(async {
            self.client
                .read()
                .await
                .send_with_options(payload, options)
                .await
        })
        .await?;
        Ok(())
    }

    /// Send a request and wait for its response. A timeout is an error.
    pub async fn request(&self, payload: Vec<u8>) -> Result<Vec<u8>, MsgtransError> {
        let response =
            on_runtime(async { self.client.read().await.request(&payload).await }).await?;
        Ok(response.to_vec())
    }

    pub async fn request_with_options(
        &self,
        payload: Vec<u8>,
        options: RequestOptions,
    ) -> Result<Vec<u8>, MsgtransError> {
        let options = request_options(options);
        let response = on_runtime(async {
            self.client
                .read()
                .await
                .request_with_options(payload.into(), options)
                .await
        })
        .await?;
        Ok(response.to_vec())
    }

    /// Wait for the next event; `null` once the client has shut down.
    ///
    /// Cancellation-safe: cancelling the call never loses an event, the next
    /// call receives it.
    ///
    /// Ending an event loop: Kotlin can cancel the coroutine. Swift cannot —
    /// UniFFI's Swift bridge does not propagate task cancellation into Rust,
    /// so a cancelled task stays suspended here until the next event. Call
    /// `shutdown()` instead, which makes every pending and later call return
    /// `nil` once buffered events are drained.
    pub async fn next_event(&self) -> Option<ClientEvent> {
        on_runtime(async {
            let mut events = self.events.lock().await;
            loop {
                // Event kinds newer than these bindings are skipped rather
                // than surfaced as something they are not.
                if let Some(event) = map_event(events.next().await?) {
                    return Some(event);
                }
            }
        })
        .await
    }
}

fn send_options(options: SendOptions) -> msgtrans::SendOptions {
    let mut out = msgtrans::SendOptions::new();
    if let Some(biz_type) = options.biz_type {
        out = out.biz_type(biz_type);
    }
    if let Some(header) = options.ext_header {
        out = out.ext_header(header.into());
    }
    out
}

fn request_options(options: RequestOptions) -> msgtrans::RequestOptions {
    let mut out = msgtrans::RequestOptions::new();
    if let Some(biz_type) = options.biz_type {
        out = out.biz_type(biz_type);
    }
    if let Some(header) = options.ext_header {
        out = out.ext_header(header.into());
    }
    if let Some(ms) = options.timeout_ms {
        out = out.timeout(std::time::Duration::from_millis(ms));
    }
    out
}

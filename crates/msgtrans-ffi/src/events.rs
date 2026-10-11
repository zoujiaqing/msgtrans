use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use bytes::Bytes;

use crate::error::MsgtransError;
use crate::runtime::{enter, on_runtime};

/// What the client observed. Delivered in order by [`crate::MsgtransClient::next_event`].
#[derive(Debug, uniffi::Enum)]
pub enum ClientEvent {
    /// A connection was established, including after a reconnect.
    Connected {
        info: ConnectionInfo,
    },
    /// A one-way message from the server.
    Message {
        message: InboundMessage,
    },
    /// A request from the server. Answer it with `IncomingRequest.respond`.
    Request {
        request: Arc<IncomingRequest>,
    },
    /// Diagnostic: a message reached the socket. Droppable under load, so it
    /// must not be used as a delivery ledger.
    MessageSent {
        message_id: u32,
    },
    Disconnected {
        reason: CloseReason,
    },
    Error {
        error: MsgtransError,
    },
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ConnectionInfo {
    pub session_id: u64,
    pub protocol: String,
    pub local_address: String,
    pub peer_address: String,
    /// Milliseconds since the Unix epoch.
    pub established_at_ms: u64,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum CloseReason {
    Normal,
    Forced,
    Timeout,
    Error {
        detail: String,
    },
    /// A close reason newer than these bindings.
    Other {
        detail: String,
    },
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct InboundMessage {
    pub message_id: u32,
    pub biz_type: u8,
    pub payload: Vec<u8>,
    pub ext_header: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RespondOutcome {
    /// The response was written to the socket.
    Written,
    /// Another response already answered this request.
    AlreadyHandled,
}

/// A request from the server, answered exactly once.
///
/// Dropping it unanswered is allowed; msgtrans then releases the request and
/// the server's call times out or fails on its side.
#[derive(Debug, uniffi::Object)]
pub struct IncomingRequest {
    request: Mutex<Option<msgtrans::ClientRequest>>,
    message_id: u32,
    biz_type: u8,
    payload: Bytes,
    ext_header: Option<Bytes>,
}

#[uniffi::export]
impl IncomingRequest {
    pub fn message_id(&self) -> u32 {
        self.message_id
    }

    pub fn biz_type(&self) -> u8 {
        self.biz_type
    }

    pub fn payload(&self) -> Vec<u8> {
        self.payload.to_vec()
    }

    pub fn ext_header(&self) -> Option<Vec<u8>> {
        self.ext_header.as_ref().map(|h| h.to_vec())
    }

    /// Answer the request. Returns once the response was written to the
    /// socket. Calling it a second time fails with `InvalidState`.
    pub async fn respond(&self, payload: Vec<u8>) -> Result<RespondOutcome, MsgtransError> {
        let request = self.take()?;
        let outcome = on_runtime(request.respond(payload)).await?;
        Ok(map_outcome(outcome))
    }

    /// Answer the request without waiting for the write.
    pub fn respond_detached(&self, payload: Vec<u8>) -> Result<(), MsgtransError> {
        let request = self.take()?;
        enter(|| request.respond_detached(payload));
        Ok(())
    }
}

impl IncomingRequest {
    fn take(&self) -> Result<msgtrans::ClientRequest, MsgtransError> {
        self.request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or_else(|| MsgtransError::InvalidState {
                detail: "this request has already been answered".to_string(),
            })
    }
}

fn map_outcome(outcome: msgtrans::RespondOutcome) -> RespondOutcome {
    match outcome {
        msgtrans::RespondOutcome::Written => RespondOutcome::Written,
        // AlreadyHandled is the only other outcome today; anything newer is
        // also "this response did not go out", which is what callers act on.
        _ => RespondOutcome::AlreadyHandled,
    }
}

/// Convert a msgtrans event. `None` for event kinds newer than these
/// bindings, which the caller skips.
pub(crate) fn map_event(event: msgtrans::ClientEvent) -> Option<ClientEvent> {
    use msgtrans::ClientEvent as E;
    Some(match event {
        E::Connected { info, .. } => ClientEvent::Connected {
            info: map_info(&info),
        },
        E::Message(message) => ClientEvent::Message {
            message: InboundMessage {
                message_id: message.message_id(),
                biz_type: message.biz_type(),
                ext_header: message.ext_header().map(<[u8]>::to_vec),
                payload: message.into_payload().to_vec(),
            },
        },
        E::Request(request) => ClientEvent::Request {
            request: Arc::new(IncomingRequest {
                message_id: request.message_id(),
                biz_type: request.biz_type(),
                payload: request.payload().clone(),
                ext_header: request.ext_header().map(Bytes::copy_from_slice),
                request: Mutex::new(Some(request)),
            }),
        },
        E::MessageSent { message_id, .. } => ClientEvent::MessageSent { message_id },
        E::Disconnected { reason, .. } => ClientEvent::Disconnected {
            reason: map_reason(reason),
        },
        E::Error { error, .. } => ClientEvent::Error {
            error: error.into(),
        },
        _ => return None,
    })
}

fn map_info(info: &msgtrans::ConnectionInfo) -> ConnectionInfo {
    ConnectionInfo {
        session_id: info.session_id.as_u64(),
        protocol: info.protocol.clone(),
        local_address: info.local_addr.to_string(),
        peer_address: info.peer_addr.to_string(),
        established_at_ms: info
            .established_at
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    }
}

fn map_reason(reason: msgtrans::CloseReason) -> CloseReason {
    match reason {
        msgtrans::CloseReason::Normal => CloseReason::Normal,
        msgtrans::CloseReason::Forced => CloseReason::Forced,
        msgtrans::CloseReason::Timeout => CloseReason::Timeout,
        msgtrans::CloseReason::Error(detail) => CloseReason::Error { detail },
        other => CloseReason::Other {
            detail: format!("{other:?}"),
        },
    }
}

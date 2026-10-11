use crate::command::ConnectionInfo;
use crate::error::TransportError;
use crate::packet::Packet;
use crate::transport::request_registry::{MarkResult, RequestRegistry};
use crate::{CloseReason, PacketId, SessionId};
use bytes::Bytes;
use std::sync::Arc;

/// Unified abstraction for transport layer events
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TransportEvent {
    /// Connection related events
    ConnectionEstablished {
        info: ConnectionInfo,
    },
    ConnectionClosed {
        reason: CloseReason,
    },

    /// Data transmission events
    MessageReceived(Packet),
    MessageSent {
        packet_id: PacketId,
    },

    /// Error events
    TransportError {
        error: TransportError,
    },
}

impl TransportEvent {
    /// Check if it's a connection related event
    pub fn is_connection_event(&self) -> bool {
        matches!(
            self,
            TransportEvent::ConnectionEstablished { .. } | TransportEvent::ConnectionClosed { .. }
        )
    }

    /// Check if it's a data transmission event
    pub fn is_data_event(&self) -> bool {
        matches!(self, TransportEvent::MessageReceived(..))
    }

    /// Check if it's an error event
    pub fn is_error_event(&self) -> bool {
        matches!(self, TransportEvent::TransportError { .. })
    }
}

/// SIMPLE User-friendly client events - completely hide Packet complexity.
///
/// Not `Clone`: `Request` carries a consuming responder that must be answered
/// exactly once.
#[derive(Debug)]
#[non_exhaustive]
pub enum ClientEvent {
    /// Connection established
    Connected { info: ConnectionInfo },
    /// Connection disconnected
    Disconnected { reason: CloseReason },

    /// A one-way message was received.
    Message(ClientMessage),
    /// A request was received; answer it via the consuming responder.
    Request(ClientRequest),

    /// Message send confirmation
    MessageSent { message_id: u32 },

    /// Transport error
    Error { error: TransportError },
}

impl ClientEvent {
    /// Convert TransportEvent to ClientEvent, hiding session ID
    pub fn from_transport_event(event: TransportEvent) -> Option<Self> {
        match event {
            TransportEvent::ConnectionEstablished { info } => Some(ClientEvent::Connected { info }),
            TransportEvent::ConnectionClosed { reason } => {
                Some(ClientEvent::Disconnected { reason })
            }
            TransportEvent::MessageReceived(packet) => {
                match packet.header.packet_type {
                    crate::packet::PacketType::Request => {
                        // Request packets are specially handled by TransportClient
                        None
                    }
                    _ => {
                        // OneWay and Response packets are plain messages.
                        let ext = if packet.ext_header.is_empty() {
                            None
                        } else {
                            Some(packet.ext_header.to_vec())
                        };
                        Some(ClientEvent::Message(ClientMessage::new(
                            None,
                            packet.header.message_id,
                            packet.header.biz_type,
                            ext,
                            packet.payload.clone(),
                        )))
                    }
                }
            }
            TransportEvent::MessageSent { packet_id } => Some(ClientEvent::MessageSent {
                message_id: packet_id,
            }),
            TransportEvent::TransportError { error } => Some(ClientEvent::Error { error }),
        }
    }

    /// Check if it's a connection related event
    pub fn is_connection_event(&self) -> bool {
        matches!(
            self,
            ClientEvent::Connected { .. } | ClientEvent::Disconnected { .. }
        )
    }

    /// Check if it's a data transmission event
    pub fn is_data_event(&self) -> bool {
        matches!(
            self,
            ClientEvent::Message(..) | ClientEvent::Request(..) | ClientEvent::MessageSent { .. }
        )
    }

    /// Check if it's an error event
    pub fn is_error_event(&self) -> bool {
        matches!(self, ClientEvent::Error { .. })
    }
}

/// What a respond call actually did.
///
/// `Ok(Written)` is the only value that means "the response bytes reached the
/// socket". Duplicate/late/unknown responds report `AlreadyHandled` instead of
/// a fake success, and real failures are `Err` — never conflated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RespondOutcome {
    /// The response was written to the transport (write-confirmed).
    Written,
    /// Nothing was written: another responder already claimed this request, or
    /// the request is no longer tracked (late/unknown). Idempotent-skip.
    AlreadyHandled,
}

/// A one-way message delivered to the client. Pure data — cloneable, with no
/// response obligation (the split mirrors the server's on_message/on_request).
#[derive(Debug, Clone)]
pub struct ClientMessage {
    peer: Option<SessionId>,
    message_id: u32,
    biz_type: u8,
    ext_header: Option<Vec<u8>>,
    data: Bytes,
}

impl ClientMessage {
    pub(crate) fn new(
        peer: Option<SessionId>,
        message_id: u32,
        biz_type: u8,
        ext_header: Option<Vec<u8>>,
        data: impl Into<Bytes>,
    ) -> Self {
        Self {
            peer,
            message_id,
            biz_type,
            ext_header,
            data: data.into(),
        }
    }

    /// Source session id (server-side); `None` on the client.
    pub fn peer(&self) -> Option<SessionId> {
        self.peer
    }
    /// Message id.
    pub fn message_id(&self) -> u32 {
        self.message_id
    }
    /// Business type.
    pub fn biz_type(&self) -> u8 {
        self.biz_type
    }
    /// Extension header, if any.
    pub fn ext_header(&self) -> Option<&[u8]> {
        self.ext_header.as_deref()
    }
    /// Payload bytes.
    pub fn payload(&self) -> &Bytes {
        &self.data
    }
    /// Take ownership of the payload.
    pub fn into_payload(self) -> Bytes {
        self.data
    }
    /// Payload as a lossy UTF-8 string.
    pub fn as_text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }
}

/// Responder closure: sends a response and reports the real write result,
/// receiving the respond claim so completion ownership travels with the queued
/// write. Boxed so `respond` can await it and `respond_detached` can spawn it.
type ResponderFn = Arc<
    dyn Fn(
            Bytes,
            crate::transport::SendOptions,
            Option<crate::transport::request_registry::RespondClaim>,
        ) -> futures::future::BoxFuture<'static, Result<(), crate::error::TransportError>>
        + Send
        + Sync,
>;

/// A request delivered to the client that carries the obligation to answer.
///
/// NOT `Clone` and consuming, exactly like the server-side `Responder`:
/// `respond(self)`/`respond_detached(self)` answer it exactly once, enforced by
/// the type system (a second response is a compile error, not a runtime dedup).
/// Dropping it without responding leaves the request to the peer's own timeout.
pub struct ClientRequest {
    peer: Option<SessionId>,
    message_id: u32,
    biz_type: u8,
    ext_header: Option<Vec<u8>>,
    data: Bytes,
    responder: ResponderFn,
    /// The registry that owns this request's lifecycle, and the unforgeable
    /// registration token. Both are REQUIRED: the registry is the only arbiter
    /// of "exactly one response per request", so a request that could not be
    /// registered must never be handed to a consumer (it is dropped upstream).
    registry: Arc<RequestRegistry>,
    token: crate::transport::request_registry::RequestToken,
}

impl ClientRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        peer: Option<SessionId>,
        message_id: u32,
        biz_type: u8,
        ext_header: Option<Vec<u8>>,
        data: impl Into<Bytes>,
        responder: ResponderFn,
        registry: Arc<RequestRegistry>,
        token: crate::transport::request_registry::RequestToken,
    ) -> Self {
        Self {
            peer,
            message_id,
            biz_type,
            ext_header,
            data: data.into(),
            responder,
            registry,
            token,
        }
    }

    /// Source session id (server-side); `None` on the client.
    pub fn peer(&self) -> Option<SessionId> {
        self.peer
    }
    /// Message id.
    pub fn message_id(&self) -> u32 {
        self.message_id
    }
    /// Business type.
    pub fn biz_type(&self) -> u8 {
        self.biz_type
    }
    /// Extension header, if any.
    pub fn ext_header(&self) -> Option<&[u8]> {
        self.ext_header.as_deref()
    }
    /// Request payload bytes.
    pub fn payload(&self) -> &Bytes {
        &self.data
    }
    /// Take ownership of the request payload. The request is thereby given up
    /// without a response and is resolved as `Dropped` when it falls out of
    /// scope (see the `Drop` impl).
    pub fn into_payload(mut self) -> Bytes {
        // `ClientRequest` has a `Drop` impl, so a field cannot be moved out;
        // swap the payload out and let the (now empty) request drop normally.
        std::mem::take(&mut self.data)
    }
    /// Request payload as a lossy UTF-8 string.
    pub fn as_text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }

    /// Respond, write-confirmed. `Ok(Written)` means the bytes reached the
    /// socket; a duplicate/late request returns `Ok(AlreadyHandled)`.
    ///
    /// Consumes the request. Cancellation-safe: the claim is created in the
    /// same poll that wins `begin_respond` and rides the queued write, so
    /// timeout/select/abort cannot strand registry state, and the token's
    /// generation check refuses cross-registration (reused-id) responses.
    pub async fn respond(
        self,
        response: impl Into<Bytes>,
    ) -> Result<RespondOutcome, crate::error::TransportError> {
        self.respond_with_options(response, crate::transport::SendOptions::new())
            .await
    }

    /// Respond with explicit [`crate::SendOptions`] — same guarantees as
    /// [`Self::respond`], plus response-side compression / ext header /
    /// `biz_type` override.
    ///
    /// `biz_type: None` inherits the request's `biz_type`. A compression
    /// failure fails the respond rather than shipping a raw payload under a
    /// compressed header.
    pub async fn respond_with_options(
        self,
        response: impl Into<Bytes>,
        options: crate::transport::SendOptions,
    ) -> Result<RespondOutcome, crate::error::TransportError> {
        let response: Bytes = response.into();
        // The registry decides whether this response may be written at all;
        // there is no bypass path. A duplicate/late/reused-id respond loses the
        // claim and reports AlreadyHandled instead of writing a second response.
        let claim = match self.registry.begin_respond(&self.token) {
            MarkResult::Updated => Some(crate::transport::request_registry::RespondClaim::new(
                self.registry.clone(),
                self.token,
            )),
            _ => return Ok(RespondOutcome::AlreadyHandled),
        };
        (self.responder)(response, options, claim)
            .await
            .map(|()| RespondOutcome::Written)
    }

    /// Respond without awaiting the outcome. Registry state still resolves
    /// truthfully (the claim rides the queued write); only the caller's
    /// visibility of the result is given up.
    pub fn respond_detached(self, response: impl Into<Bytes>) {
        let response: Bytes = response.into();
        tokio::spawn(async move {
            if let Err(e) = self.respond(response).await {
                tracing::debug!("[RESPOND] detached response send failed: {:?}", e);
            }
        });
    }
}

impl Drop for ClientRequest {
    fn drop(&mut self) {
        // Deterministic cleanup: an inbound request dropped without a response
        // (the consumer ignored it, took only its payload, or was cancelled) is
        // resolved as `Dropped` now instead of lingering `Pending` until a
        // scanner (clients run one only as a fallback). Generation-aware, so a
        // reused id is never touched; a no-op once `respond`/`respond_detached`
        // advanced the entry past `Pending`.
        self.registry.abort_request_token(&self.token);
    }
}

impl std::fmt::Debug for ClientRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientRequest")
            .field("peer", &self.peer)
            .field("message_id", &self.message_id)
            .field("biz_type", &self.biz_type)
            .field("data", &format!("{} bytes", self.data.len()))
            .finish()
    }
}

/// Proof that ONE message was handed to the transport, and its id.
///
/// Returned by the send paths. What it proves depends on which one produced it:
/// `send()` returns it only after the bytes were **written** (write-confirmed),
/// while `send_detached()` returns it once the message is **queued**. It
/// replaces the 1.x `TransportResult`/`TransportStatus` pair, which could encode
/// impossible combinations (e.g. `status: Timeout` together with response data,
/// or `Sent` with no send having been confirmed): failures are now `Err`, and a
/// request's response is simply the returned `Bytes`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SendReceipt {
    /// Target session (server side); `None` on the client's single connection.
    pub peer: Option<SessionId>,
    /// The id the transport assigned to this message.
    pub message_id: u32,
}

impl SendReceipt {
    pub(crate) fn new(peer: Option<SessionId>, message_id: u32) -> Self {
        Self { peer, message_id }
    }
}

#[cfg(test)]
mod respond_tests {
    use super::*;
    use crate::transport::request_registry::RequestToken;

    fn tracked(sid: u64, id: u32) -> (Arc<RequestRegistry>, RequestToken) {
        let registry = Arc::new(RequestRegistry::new());
        registry.open_session(SessionId(sid));
        let token = registry
            .register(
                id,
                Some(SessionId(sid)),
                0,
                std::time::Duration::from_secs(5),
            )
            .expect("registers");
        (registry, token)
    }

    fn responder_with(fut_claim_hold: bool) -> ResponderFn {
        // fut_claim_hold=true: hold the claim inside a never-completing future,
        // modelling a send stuck in the outbound path.
        Arc::new(move |_data, _options, claim| {
            if fut_claim_hold {
                Box::pin(async move {
                    let _hold = claim;
                    std::future::pending::<()>().await;
                    unreachable!()
                })
            } else {
                Box::pin(async move {
                    if let Some(claim) = claim {
                        claim.resolve(true);
                    }
                    Ok(())
                })
            }
        })
    }

    fn tracked_ctx(
        registry: &Arc<RequestRegistry>,
        token: RequestToken,
        responder: ResponderFn,
    ) -> ClientRequest {
        ClientRequest::new(
            Some(SessionId(7)),
            1,
            0,
            None,
            b"req".to_vec(),
            responder,
            registry.clone(),
            token,
        )
    }

    #[tokio::test]
    async fn respond_returns_written_from_responder() {
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, responder_with(false));
        assert_eq!(
            ctx.respond(b"resp".to_vec()).await.unwrap(),
            RespondOutcome::Written
        );
    }

    /// `respond_with_options` must hand the options to the write path — the
    /// whole point of the method. If they were dropped, a caller asking for
    /// compression would silently get an uncompressed response.
    #[tokio::test]
    async fn respond_with_options_forwards_the_options() {
        let seen = Arc::new(std::sync::Mutex::new(None));
        let sink = seen.clone();
        let capture: ResponderFn = Arc::new(move |_data, options, claim| {
            *sink.lock().unwrap() = Some(options);
            if let Some(claim) = claim {
                claim.resolve(true);
            }
            Box::pin(async { Ok(()) })
        });
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, capture);
        let options = crate::transport::SendOptions::new()
            .biz_type(9)
            .compression(crate::packet::CompressionType::Zstd);
        assert_eq!(
            ctx.respond_with_options(b"resp".to_vec(), options)
                .await
                .unwrap(),
            RespondOutcome::Written
        );
        let observed = seen.lock().unwrap().clone().expect("responder was called");
        assert_eq!(observed.biz_type, Some(9));
        assert_eq!(
            observed.compression,
            Some(crate::packet::CompressionType::Zstd)
        );
    }

    #[tokio::test]
    async fn respond_propagates_send_error() {
        let err: ResponderFn = Arc::new(|_data, _options, _claim| {
            Box::pin(async {
                // _claim drops here: a failed send resolves the registry
                // entry as SendFailed by construction.
                Err(crate::error::TransportError::connection_error(
                    "boom", false,
                ))
            })
        });
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, err);
        assert!(ctx.respond(b"resp".to_vec()).await.is_err());
        assert_eq!(registry.counters_snapshot().response_send_failed_total, 1);
    }

    // (The former `respond_on_one_way_is_error` test is gone: a one-way message
    // is now a `ClientMessage`, which has no `respond` method at all, so the
    // error is a compile error by construction rather than a runtime check.)

    /// Dropping a `ClientRequest` without responding must deterministically
    /// resolve the inbound entry (Pending -> Dropped), not leak it until a
    /// scanner. Covers the "consumer ignored the request" path.
    #[tokio::test]
    async fn dropping_unanswered_request_resolves_it() {
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, responder_with(false));
        assert_eq!(registry.counters_snapshot().pending_requests, 1);
        drop(ctx); // never responded
        assert_eq!(
            registry.get_state(
                Some(SessionId(7)),
                1,
                crate::transport::request_registry::RequestDirection::Inbound
            ),
            None,
            "a dropped-unanswered request must not stay pending"
        );
        assert_eq!(registry.counters_snapshot().pending_requests, 0);
    }

    /// `into_payload` gives up the request without answering: same deterministic
    /// resolution as a bare drop (it must not leave a `Pending` entry).
    #[tokio::test]
    async fn into_payload_resolves_the_request() {
        let (registry, token) = tracked(7, 2);
        let ctx = crate::event::ClientRequest::new(
            Some(SessionId(7)),
            2,
            0,
            None,
            b"body".to_vec(),
            responder_with(false),
            registry.clone(),
            token,
        );
        assert_eq!(registry.counters_snapshot().pending_requests, 1);
        let payload = ctx.into_payload();
        assert_eq!(&payload[..], b"body");
        assert_eq!(registry.counters_snapshot().pending_requests, 0);
    }

    /// Cancelling a respond mid-send must not strand the registry in
    /// Responding: the claim travels with the responder future, so dropping
    /// the caller's future resolves the entry as a send failure.
    #[tokio::test]
    async fn cancelled_respond_resolves_the_claim() {
        let (registry, token) = tracked(7, 1);
        let ctx = tracked_ctx(&registry, token, responder_with(true));

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            ctx.respond(b"resp".to_vec()),
        )
        .await;
        assert!(
            cancelled.is_err(),
            "send must still be in flight at timeout"
        );
        // The timeout dropped the respond future -> claim dropped -> resolved.
        assert_eq!(
            registry.get_state(
                Some(SessionId(7)),
                1,
                crate::transport::request_registry::RequestDirection::Inbound
            ),
            None,
            "cancelled respond must not leave a Responding entry"
        );
        let snapshot = registry.counters_snapshot();
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.response_send_failed_total, 1);
        assert_eq!(registry.active_len(), 0);
    }

    /// A context whose token belongs to an earlier registration of a reused
    /// message id reports AlreadyHandled — it can never claim the new one.
    #[tokio::test]
    async fn stale_context_reports_already_handled_after_id_reuse() {
        let (registry, old_token) = tracked(7, 1);
        let stale = tracked_ctx(&registry, old_token, responder_with(false));

        // First life ends and the id is reused by a new registration.
        assert_eq!(registry.begin_respond(&old_token), MarkResult::Updated);
        assert_eq!(
            registry.finish_respond(&old_token, true),
            MarkResult::Updated
        );
        let new_token = registry
            .register(1, Some(SessionId(7)), 0, std::time::Duration::from_secs(5))
            .expect("re-registers");

        assert_eq!(
            stale.respond(b"late".to_vec()).await.unwrap(),
            RespondOutcome::AlreadyHandled
        );
        // The new registration is untouched and answerable.
        assert_eq!(registry.begin_respond(&new_token), MarkResult::Updated);
        assert_eq!(
            registry.finish_respond(&new_token, true),
            MarkResult::Updated
        );
    }
}

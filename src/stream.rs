use crate::{event::TransportEvent, packet::Packet, SessionId};
use futures::{Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::BroadcastStream;

/// Unified event stream
///
/// Provides streaming access to transport events, supporting filtering and transformation
pub struct EventStream {
    /// Internal broadcast stream
    inner: BroadcastStream<TransportEvent>,
    /// Optional session filter
    session_filter: Option<SessionId>,
}

impl EventStream {
    /// Create new event stream
    pub fn new(receiver: broadcast::Receiver<TransportEvent>) -> Self {
        Self {
            inner: BroadcastStream::new(receiver),
            session_filter: None,
        }
    }

    /// Create event stream with session filter
    pub fn with_session_filter(
        receiver: broadcast::Receiver<TransportEvent>,
        session_id: SessionId,
    ) -> Self {
        Self {
            inner: BroadcastStream::new(receiver),
            session_filter: Some(session_id),
        }
    }

    /// Filter events for specific session
    pub fn filter_session(mut self, session_id: SessionId) -> Self {
        self.session_filter = Some(session_id);
        self
    }

    /// Get next connection event
    pub async fn next_connection_event(&mut self) -> Option<TransportEvent> {
        while let Some(result) = self.inner.next().await {
            if let Ok(event) = result {
                if self.should_emit_event(&event) && event.is_connection_event() {
                    return Some(event);
                }
            }
        }
        None
    }

    /// Get next data event
    pub async fn next_data_event(&mut self) -> Option<TransportEvent> {
        while let Some(result) = self.inner.next().await {
            if let Ok(event) = result {
                if self.should_emit_event(&event) && event.is_data_event() {
                    return Some(event);
                }
            }
        }
        None
    }

    /// Get next error event
    pub async fn next_error_event(&mut self) -> Option<TransportEvent> {
        while let Some(result) = self.inner.next().await {
            if let Ok(event) = result {
                if self.should_emit_event(&event) && event.is_error_event() {
                    return Some(event);
                }
            }
        }
        None
    }

    /// Check if event should be emitted (based on filter)
    fn should_emit_event(&self, event: &TransportEvent) -> bool {
        match self.session_filter {
            Some(filter_session_id) => match event.session_id() {
                Some(event_session_id) => event_session_id == filter_session_id,
                None => false,
            },
            None => true, // No filter, emit all events
        }
    }
}

impl Stream for EventStream {
    type Item = TransportEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if self.should_emit_event(&event) {
                        return Poll::Ready(Some(event));
                    }
                    // Continue loop, get next event
                }
                Poll::Ready(Some(Err(_))) => {
                    // Ignore receive errors, continue trying
                    continue;
                }
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

/// Generic receiver
///
/// Solves Future state management issues in the original Stream trait implementation
/// Avoids recreating Future on poll_next by maintaining Future state
pub struct GenericReceiver<T> {
    /// Internal receiver
    receiver: mpsc::Receiver<T>,
    /// Current receive Future (may be None)
    #[allow(dead_code)]
    recv_future: Option<Pin<Box<dyn std::future::Future<Output = Option<T>> + Send>>>,
}

impl<T: Send + 'static> GenericReceiver<T> {
    /// Create new generic receiver
    pub fn new(receiver: mpsc::Receiver<T>) -> Self {
        Self {
            receiver,
            recv_future: None,
        }
    }

    /// Non-blocking attempt to receive
    pub fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    /// Asynchronous receive
    pub async fn recv(&mut self) -> Option<T> {
        self.receiver.recv().await
    }

    /// Close receiver
    pub fn close(&mut self) {
        self.receiver.close();
    }
}

impl<T: Send + 'static> Stream for GenericReceiver<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Poll receiver directly
        self.receiver.poll_recv(cx)
    }
}

/// Packet stream
///
/// Stream specifically for handling packets
pub struct PacketStream {
    /// Internal event stream
    event_stream: EventStream,
}

impl PacketStream {
    /// Create new packet stream
    pub fn new(receiver: broadcast::Receiver<TransportEvent>) -> Self {
        Self {
            event_stream: EventStream::new(receiver),
        }
    }

    /// Create packet stream with session filter
    pub fn with_session_filter(
        receiver: broadcast::Receiver<TransportEvent>,
        session_id: SessionId,
    ) -> Self {
        Self {
            event_stream: EventStream::with_session_filter(receiver, session_id),
        }
    }

    /// Get next packet
    pub async fn next_packet(&mut self) -> Option<Packet> {
        while let Some(event) = self.event_stream.next().await {
            if let crate::event::TransportEvent::MessageReceived(packet) = event {
                return Some(packet);
            }
        }
        None
    }
}

impl Stream for PacketStream {
    type Item = Packet;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.event_stream).poll_next(cx) {
                Poll::Ready(Some(crate::event::TransportEvent::MessageReceived(packet))) => {
                    return Poll::Ready(Some(packet));
                }
                Poll::Ready(Some(_)) => {
                    continue;
                }
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

/// Connection event stream
///
/// Specifically handles connection establishment and closing events
pub struct ConnectionStream {
    /// Internal event stream
    event_stream: EventStream,
}

impl ConnectionStream {
    /// Create new connection event stream
    pub fn new(receiver: broadcast::Receiver<TransportEvent>) -> Self {
        Self {
            event_stream: EventStream::new(receiver),
        }
    }

    /// Get next connection event
    pub async fn next_connection(&mut self) -> Option<ConnectionEvent> {
        while let Some(event) = self.event_stream.next().await {
            match event {
                TransportEvent::ConnectionEstablished { info } => {
                    return Some(ConnectionEvent::Established { info });
                }
                TransportEvent::ConnectionClosed { reason } => {
                    return Some(ConnectionEvent::Closed { reason });
                }
                _ => continue,
            }
        }
        None
    }
}

impl Stream for ConnectionStream {
    type Item = ConnectionEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.event_stream).poll_next(cx) {
                Poll::Ready(Some(TransportEvent::ConnectionEstablished { info })) => {
                    return Poll::Ready(Some(ConnectionEvent::Established { info }));
                }
                Poll::Ready(Some(TransportEvent::ConnectionClosed { reason })) => {
                    return Poll::Ready(Some(ConnectionEvent::Closed { reason }));
                }
                Poll::Ready(Some(_)) => {
                    // Other events, continue loop
                    continue;
                }
                Poll::Ready(None) => {
                    return Poll::Ready(None);
                }
                Poll::Pending => {
                    return Poll::Pending;
                }
            }
        }
    }
}

/// Connection event
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    /// Connection established
    Established {
        info: crate::command::ConnectionInfo,
    },
    /// Connection closed
    Closed { reason: crate::CloseReason },
}

/// Stream factory
///
/// Provides convenient methods for creating various types of streams
pub struct StreamFactory;

impl StreamFactory {
    /// Create event stream
    pub fn event_stream(receiver: broadcast::Receiver<TransportEvent>) -> EventStream {
        EventStream::new(receiver)
    }

    /// Create packet stream
    pub fn packet_stream(receiver: broadcast::Receiver<TransportEvent>) -> PacketStream {
        PacketStream::new(receiver)
    }

    /// Create connection stream
    pub fn connection_stream(receiver: broadcast::Receiver<TransportEvent>) -> ConnectionStream {
        ConnectionStream::new(receiver)
    }

    /// Create session-filtered event stream
    pub fn session_event_stream(
        receiver: broadcast::Receiver<TransportEvent>,
        session_id: SessionId,
    ) -> EventStream {
        EventStream::with_session_filter(receiver, session_id)
    }

    /// Create session-filtered packet stream
    pub fn session_packet_stream(
        receiver: broadcast::Receiver<TransportEvent>,
        session_id: SessionId,
    ) -> PacketStream {
        PacketStream::with_session_filter(receiver, session_id)
    }

    /// Create client event stream (hide session ID)
    pub fn client_event_stream(
        receiver: tokio::sync::broadcast::Receiver<crate::event::TransportEvent>,
    ) -> ClientEventStream {
        ClientEventStream::new(receiver)
    }
}

/// Stream combinator
///
/// Provides stream combination and transformation functionality
pub struct StreamCombinator;

impl StreamCombinator {
    /// Merge multiple event streams
    pub fn merge_event_streams(streams: Vec<EventStream>) -> impl Stream<Item = TransportEvent> {
        futures::stream::select_all(streams)
    }

    /// Convert event stream to packet stream
    pub fn events_to_packets(event_stream: EventStream) -> impl Stream<Item = Packet> {
        event_stream.filter_map(|event| async move {
            match event {
                crate::event::TransportEvent::MessageReceived(packet) => Some(packet),
                _ => None,
            }
        })
    }

    /// Convert event stream to connection event stream
    pub fn events_to_connections(event_stream: EventStream) -> impl Stream<Item = ConnectionEvent> {
        event_stream.filter_map(|event| async move {
            match event {
                TransportEvent::ConnectionEstablished { info } => {
                    Some(ConnectionEvent::Established { info })
                }
                TransportEvent::ConnectionClosed { reason } => {
                    Some(ConnectionEvent::Closed { reason })
                }
                _ => None,
            }
        })
    }
}

// Add extension trait for broadcast::Receiver
pub trait ReceiverExt {
    /// Convert to event stream
    fn into_event_stream(self) -> EventStream;

    /// Convert to packet stream
    fn into_packet_stream(self) -> PacketStream;

    /// Convert to connection stream
    fn into_connection_stream(self) -> ConnectionStream;
}

impl ReceiverExt for broadcast::Receiver<TransportEvent> {
    fn into_event_stream(self) -> EventStream {
        EventStream::new(self)
    }

    fn into_packet_stream(self) -> PacketStream {
        PacketStream::new(self)
    }

    fn into_connection_stream(self) -> ConnectionStream {
        ConnectionStream::new(self)
    }
}

/// Client event stream - hides session ID concept
pub struct ClientEventStream {
    inner: tokio::sync::broadcast::Receiver<crate::event::TransportEvent>,
}

impl ClientEventStream {
    /// Create new client event stream
    pub fn new(receiver: tokio::sync::broadcast::Receiver<crate::event::TransportEvent>) -> Self {
        Self { inner: receiver }
    }

    /// Receive next client event
    pub async fn next(
        &mut self,
    ) -> Result<crate::event::ClientEvent, crate::error::TransportError> {
        loop {
            match self.inner.recv().await {
                Ok(transport_event) => {
                    // Convert to client event, filter out irrelevant events
                    if let Some(client_event) =
                        crate::event::ClientEvent::from_transport_event(transport_event)
                    {
                        return Ok(client_event);
                    }
                    // If it's an irrelevant event, continue looping to wait for next event
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(crate::error::TransportError::connection_error(
                        "Event stream closed",
                        false,
                    ));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Event stream lagged, continue receiving
                    continue;
                }
            }
        }
    }

    /// Try to receive event (non-blocking)
    pub fn try_next(
        &mut self,
    ) -> Result<Option<crate::event::ClientEvent>, crate::error::TransportError> {
        loop {
            match self.inner.try_recv() {
                Ok(transport_event) => {
                    // Convert to client event, filter out irrelevant events
                    if let Some(client_event) =
                        crate::event::ClientEvent::from_transport_event(transport_event)
                    {
                        return Ok(Some(client_event));
                    }
                    // If it's an irrelevant event, continue looping
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    return Ok(None);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    return Err(crate::error::TransportError::connection_error(
                        "Event stream closed",
                        false,
                    ));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    // Event stream lagged, continue receiving
                    continue;
                }
            }
        }
    }
}

//! Proves an out-of-crate adapter can fully implement the transport SPI using
//! only `msgtrans::spi` — no private types. If this compiles, the SPI is real.

use async_trait::async_trait;
use msgtrans::spi::{
    event_channel, CloseReason, Connection, ConnectionEvents, ConnectionInfo, EventSink, Packet,
    SessionId, TransportError, TransportEvent, WriteCompletion,
};

struct MyConnection {
    session_id: SessionId,
    connected: bool,
    sink: EventSink,
    events: Option<ConnectionEvents>,
}

impl MyConnection {
    fn new() -> Self {
        let (sink, events) = event_channel(1024);
        Self {
            session_id: SessionId::new(0),
            connected: true,
            sink,
            events: Some(events),
        }
    }
}

#[async_trait]
impl Connection for MyConnection {
    async fn send_with_completion(
        &mut self,
        _packet: Packet,
        completion: WriteCompletion,
    ) -> Result<(), TransportError> {
        // A real adapter enqueues and resolves from its write loop; here we
        // confirm immediately to exercise the completion API.
        completion.complete(Ok(()));
        Ok(())
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        self.connected = false;
        self.sink.close(CloseReason::Normal);
        Ok(())
    }
    fn session_id(&self) -> SessionId {
        self.session_id
    }
    fn set_session_id(&mut self, session_id: SessionId) {
        self.session_id = session_id;
    }
    fn connection_info(&self) -> ConnectionInfo {
        ConnectionInfo::default()
    }
    fn is_connected(&self) -> bool {
        self.connected
    }
    async fn flush(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
    fn take_event_pipe(&mut self) -> Option<ConnectionEvents> {
        self.events.take()
    }
}

#[tokio::test]
async fn external_connection_impl_drives_the_event_sink() {
    let mut conn = MyConnection::new();
    let mut events = conn.take_event_pipe().expect("events taken once");

    // The adapter pushes a data event through the public sink...
    assert!(
        conn.sink
            .deliver(TransportEvent::MessageReceived(Packet::one_way(
                1,
                b"hi".to_vec()
            )))
            .await
    );
    // ...the consumer sees it, then exactly one ConnectionClosed after close().
    assert!(matches!(
        events.next().await,
        Some(TransportEvent::MessageReceived(_))
    ));
    conn.close().await.unwrap();
    assert!(matches!(
        events.next().await,
        Some(TransportEvent::ConnectionClosed {
            reason: CloseReason::Normal
        })
    ));
    assert!(events.next().await.is_none());
}

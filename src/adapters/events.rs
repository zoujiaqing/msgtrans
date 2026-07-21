//! Bounded event backbone between a connection adapter and its consumer.
//!
//! Replaces the per-connection `broadcast` channel on the data path. The two
//! planes are deliberately separate:
//!
//! - **Data plane** (`deliver`): a bounded, single-consumer mpsc queue. A slow
//!   consumer backpressures the adapter's read loop (and through it the
//!   socket), instead of silently skipping events the way a lagging broadcast
//!   receiver did.
//! - **Control plane** (`close`): a `watch` cell. Setting it never blocks and
//!   never queues behind data, so the close signal cannot be lost to a full
//!   data queue. The consumer still observes ordered delivery: queued data
//!   first, then exactly one `ConnectionClosed`.
//!
//! Diagnostics (`MessageSent` confirmations) use `try_send` and are droppable
//! under load — that is the only tier where loss is acceptable, and it is
//! explicit here rather than an accident of broadcast lag.

use crate::error::CloseReason;
use crate::event::TransportEvent;
use tokio::sync::{mpsc, watch};

/// Adapter-side sender half. Wrap in `Arc` to share between read/write tasks
/// (`watch::Sender` is not `Clone`).
#[derive(Debug)]
pub struct EventPipe {
    data_tx: mpsc::Sender<TransportEvent>,
    close_tx: watch::Sender<Option<CloseReason>>,
}

/// Consumer-side receiver half. Single consumer, taken from the connection
/// exactly once.
#[derive(Debug)]
pub struct EventPipeRx {
    data_rx: mpsc::Receiver<TransportEvent>,
    close_rx: watch::Receiver<Option<CloseReason>>,
    close_emitted: bool,
}

/// Create a connected pipe with the given data-plane capacity.
pub fn event_pipe(capacity: usize) -> (EventPipe, EventPipeRx) {
    let (data_tx, data_rx) = mpsc::channel(capacity);
    let (close_tx, close_rx) = watch::channel(None);
    (
        EventPipe { data_tx, close_tx },
        EventPipeRx {
            data_rx,
            close_rx,
            close_emitted: false,
        },
    )
}

impl EventPipe {
    /// Deliver a data-plane event with backpressure. Returns `false` when the
    /// consumer is gone — the adapter should stop reading.
    pub async fn deliver(&self, event: TransportEvent) -> bool {
        self.data_tx.send(event).await.is_ok()
    }

    /// Emit a droppable diagnostic (send confirmations). Never blocks; under
    /// load the diagnostic is dropped, never a data message.
    pub fn diagnostic(&self, event: TransportEvent) {
        let _ = self.data_tx.try_send(event);
    }

    /// Publish the terminal close reason on the control plane. Non-blocking
    /// and immune to data-queue pressure. The adapter should drop the pipe
    /// (or let its tasks end) afterwards so the consumer sees end-of-data.
    pub fn close(&self, reason: CloseReason) {
        // send_replace, not send: the watch must update even if the consumer
        // has not subscribed yet or has already dropped.
        let _ = self.close_tx.send_replace(Some(reason));
    }
}

impl EventPipeRx {
    /// Next event in order: every queued data event first, then exactly one
    /// `ConnectionClosed` (reason from the control plane, or an abnormal-end
    /// reason if the adapter died without closing), then `None` forever.
    pub async fn next(&mut self) -> Option<TransportEvent> {
        if let Some(event) = self.data_rx.recv().await {
            return Some(event);
        }
        if self.close_emitted {
            return None;
        }
        self.close_emitted = true;
        let reason =
            self.close_rx.borrow().clone().unwrap_or_else(|| {
                CloseReason::Error("connection ended without close".to_string())
            });
        Some(TransportEvent::ConnectionClosed { reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::Packet;

    fn msg(id: u32) -> TransportEvent {
        TransportEvent::MessageReceived(Packet::one_way(id, vec![id as u8]))
    }

    #[tokio::test]
    async fn delivers_data_then_exactly_one_close_then_none() {
        let (tx, mut rx) = event_pipe(8);
        assert!(tx.deliver(msg(1)).await);
        tx.close(CloseReason::Normal);
        drop(tx);

        assert!(matches!(
            rx.next().await,
            Some(TransportEvent::MessageReceived(p)) if p.message_id() == 1
        ));
        assert!(matches!(
            rx.next().await,
            Some(TransportEvent::ConnectionClosed {
                reason: CloseReason::Normal
            })
        ));
        assert!(rx.next().await.is_none());
        assert!(rx.next().await.is_none());
    }

    #[tokio::test]
    async fn close_survives_a_full_data_queue() {
        // The failure mode this backbone exists to kill: with broadcast, a
        // full/lagging consumer lost ConnectionClosed. Here the close rides
        // the watch cell, so it lands even when the data queue is at capacity
        // the whole time.
        let (tx, mut rx) = event_pipe(1);
        assert!(tx.deliver(msg(1)).await); // queue now full
        tx.close(CloseReason::Timeout); // must not block or get lost
        drop(tx);

        assert!(matches!(
            rx.next().await,
            Some(TransportEvent::MessageReceived(_))
        ));
        assert!(matches!(
            rx.next().await,
            Some(TransportEvent::ConnectionClosed {
                reason: CloseReason::Timeout
            })
        ));
    }

    #[tokio::test]
    async fn abnormal_end_without_close_synthesizes_error_reason() {
        let (tx, mut rx) = event_pipe(4);
        drop(tx); // adapter died without calling close()
        assert!(matches!(
            rx.next().await,
            Some(TransportEvent::ConnectionClosed {
                reason: CloseReason::Error(_)
            })
        ));
        assert!(rx.next().await.is_none());
    }

    #[tokio::test]
    async fn deliver_backpressures_and_diagnostic_drops() {
        let (tx, mut rx) = event_pipe(1);
        assert!(tx.deliver(msg(1)).await); // full
                                           // Diagnostic on a full queue is dropped, not blocked.
        tx.diagnostic(TransportEvent::MessageSent { packet_id: 9 });
        // deliver() blocks until the consumer drains one.
        {
            let deliver = tx.deliver(msg(2));
            tokio::pin!(deliver);
            assert!(futures::poll!(&mut deliver).is_pending());
            assert!(rx.next().await.is_some()); // drain msg(1)
            assert!(deliver.await); // now completes
        }
        // The dropped diagnostic never shows up.
        tx.close(CloseReason::Normal);
        drop(tx);
        assert!(
            matches!(rx.next().await, Some(TransportEvent::MessageReceived(p)) if p.message_id() == 2)
        );
        assert!(matches!(
            rx.next().await,
            Some(TransportEvent::ConnectionClosed { .. })
        ));
    }
}

//! Shared outbound-queue send policy for protocol adapters.
//!
//! All adapters bound their outbound queue to cap per-connection memory. The
//! question is what `send()` does once that queue is full:
//!
//! - Blocking forever gives clean backpressure but lets a single stuck peer
//!   stall a fan-out loop indefinitely (head-of-line blocking).
//! - Failing immediately avoids that stall, but turns ordinary traffic bursts
//!   into spurious errors, because a healthy link drains the queue in
//!   milliseconds.
//!
//! We take the middle ground: wait a bounded amount of time. Transient bursts
//! are absorbed like before, while a genuinely stuck consumer fails fast and
//! cannot block the caller indefinitely.

use crate::connection::WriteCompletion;
use crate::error::TransportError;
use crate::packet::Packet;
use std::time::Duration;
use tokio::sync::mpsc::{self, error::SendTimeoutError};
use tokio::sync::oneshot;

/// One outbound queue item: the packet, plus an optional write completion.
///
/// `completion: None` is the fire-and-forget tier (enqueue success is the only
/// signal). `completion: Some` is the confirmed tier: the write loop resolves
/// it with the REAL write result after the socket write — "accepted for
/// delivery" and "delivered to the socket" are no longer conflated, and
/// dropping the item anywhere along the way reports failure by construction.
pub(crate) struct Outbound {
    pub packet: Packet,
    pub completion: Option<WriteCompletion>,
}

impl Outbound {
    pub fn fire_and_forget(packet: Packet) -> Self {
        Self {
            packet,
            completion: None,
        }
    }
}

/// Depth of each connection's outbound queue.
///
/// Bounds per-connection memory while leaving enough headroom to absorb bursts.
/// Measured under a 200-connection unpaced flood (`load_test -m send -c 200
/// -i 0`): a depth of 512 fails continuously from mid-run (~53 errors), 2048
/// sustains the whole flood with a handful of errors only at wind-down, and
/// 8192 is error-free. 2048 keeps a 4x memory reduction over the previous 8192
/// without the mid-flight failures.
pub(crate) const SEND_QUEUE_CAPACITY: usize = 2048;

/// How long `send()` waits for outbound queue space before giving up.
///
/// Sized well above the drain time of a healthy link (the whole queue clears in
/// milliseconds at observed throughput) and well below anything a caller would
/// consider a stall.
pub(crate) const SEND_QUEUE_WAIT: Duration = Duration::from_millis(100);

/// Enqueue a packet with bounded backpressure.
///
/// Returns a resource error (carrying the real queued depth) if the queue stays
/// full for [`SEND_QUEUE_WAIT`], or a connection error if the peer is gone.
pub(crate) async fn send_bounded(
    queue: &mpsc::Sender<Outbound>,
    packet: Packet,
    queue_name: &'static str,
    closed_msg: &'static str,
) -> Result<(), TransportError> {
    send_item(
        queue,
        Outbound::fire_and_forget(packet),
        queue_name,
        closed_msg,
    )
    .await
}

/// Enqueue a packet carrying a write completion, WITHOUT awaiting anything.
/// Callers that hold a lock for the enqueue must await their observer receipt
/// after releasing it, or one slow write would serialize every other sender.
/// On enqueue failure the completion is dropped with the rejected item, which
/// reports the failure to its observer and claim by construction.
pub(crate) async fn send_with_completion_bounded(
    queue: &mpsc::Sender<Outbound>,
    packet: Packet,
    completion: WriteCompletion,
    queue_name: &'static str,
    closed_msg: &'static str,
) -> Result<(), TransportError> {
    // Fire-and-forget completions carry no observer and no claim: skip the
    // per-item Option payload entirely so the single SPI entry point costs
    // the same as the old dedicated send() path.
    let completion = if completion.is_detached() {
        None
    } else {
        Some(completion)
    };
    send_item(
        queue,
        Outbound { packet, completion },
        queue_name,
        closed_msg,
    )
    .await
}

/// Await a write receipt; a dropped sender (connection ended before the
/// write) is a connection error, not silence.
pub(crate) async fn await_receipt(
    rx: oneshot::Receiver<Result<(), TransportError>>,
    closed_msg: &'static str,
) -> Result<(), TransportError> {
    match rx.await {
        Ok(result) => result,
        Err(_) => Err(TransportError::connection_error(closed_msg, false)),
    }
}

async fn send_item(
    queue: &mpsc::Sender<Outbound>,
    item: Outbound,
    queue_name: &'static str,
    closed_msg: &'static str,
) -> Result<(), TransportError> {
    match queue.send_timeout(item, SEND_QUEUE_WAIT).await {
        Ok(()) => Ok(()),
        Err(SendTimeoutError::Timeout(_)) => Err(TransportError::resource_error(
            queue_name,
            // `capacity()` is the remaining slots, so this is the queued depth.
            queue.max_capacity() - queue.capacity(),
            queue.max_capacity(),
        )),
        Err(SendTimeoutError::Closed(_)) => {
            Err(TransportError::connection_error(closed_msg, false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(id: u32) -> Packet {
        Packet::one_way(id, vec![0u8; 4])
    }

    #[tokio::test]
    async fn sends_immediately_when_queue_has_room() {
        let (tx, _rx) = mpsc::channel::<Outbound>(2);
        assert!(send_bounded(&tx, packet(1), "q", "closed").await.is_ok());
    }

    /// The regression guard: a burst that briefly fills the queue must still be
    /// delivered once the writer drains it, not rejected outright.
    #[tokio::test]
    async fn absorbs_transient_burst_instead_of_failing() {
        let (tx, mut rx) = mpsc::channel::<Outbound>(1);
        tx.send(Outbound::fire_and_forget(packet(1))).await.unwrap();

        // Writer drains shortly after, well within SEND_QUEUE_WAIT.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = rx.recv().await;
            // Hold the receiver so the channel stays open.
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        assert!(send_bounded(&tx, packet(2), "q", "closed").await.is_ok());
    }

    #[tokio::test]
    async fn reports_real_depth_when_queue_stays_full() {
        let (tx, _rx) = mpsc::channel::<Outbound>(2);
        tx.send(Outbound::fire_and_forget(packet(1))).await.unwrap();
        tx.send(Outbound::fire_and_forget(packet(2))).await.unwrap();

        let error = send_bounded(&tx, packet(3), "tcp_outbound_queue", "closed")
            .await
            .unwrap_err();

        assert!(
            matches!(
                error,
                TransportError::Resource { ref resource, current: 2, limit: 2 }
                    if resource == "tcp_outbound_queue"
            ),
            "unexpected error: {error:?}"
        );
    }

    #[tokio::test]
    async fn reports_connection_closed_when_peer_is_gone() {
        let (tx, rx) = mpsc::channel::<Outbound>(1);
        drop(rx);

        let error = send_bounded(&tx, packet(1), "q", "TCP connection closed")
            .await
            .unwrap_err();

        assert!(
            matches!(error, TransportError::Connection { .. }),
            "unexpected error: {error:?}"
        );
    }

    async fn send_confirmed_for_test(
        tx: &mpsc::Sender<Outbound>,
        p: Packet,
    ) -> Result<(), TransportError> {
        let (otx, orx) = oneshot::channel();
        send_with_completion_bounded(tx, p, WriteCompletion::new(Some(otx), None), "q", "closed")
            .await?;
        await_receipt(orx, "closed").await
    }

    /// Confirmed tier: Ok only after the writer resolves the actual write.
    #[tokio::test]
    async fn confirmed_send_reports_the_write_result() {
        let (tx, mut rx) = mpsc::channel::<Outbound>(2);
        let writer = tokio::spawn(async move {
            let item = rx.recv().await.expect("item");
            item.completion.expect("completion").complete(Ok(()));
            let item = rx.recv().await.expect("item");
            item.completion
                .expect("completion")
                .complete(Err(TransportError::connection_error("write failed", false)));
        });
        assert!(send_confirmed_for_test(&tx, packet(1)).await.is_ok());
        assert!(send_confirmed_for_test(&tx, packet(2)).await.is_err());
        writer.await.unwrap();
    }

    /// Writer dying before the write is a connection error, not silence: the
    /// dropped completion closes the observer channel by construction.
    #[tokio::test]
    async fn confirmed_send_fails_when_writer_drops_the_completion() {
        let (tx, mut rx) = mpsc::channel::<Outbound>(1);
        let writer = tokio::spawn(async move {
            let item = rx.recv().await.expect("item");
            drop(item.completion); // connection died before the write
        });
        let err = send_confirmed_for_test(&tx, packet(1)).await.unwrap_err();
        assert!(matches!(err, TransportError::Connection { .. }));
        writer.await.unwrap();
    }
}

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

use crate::error::TransportError;
use crate::packet::Packet;
use std::time::Duration;
use tokio::sync::mpsc::{self, error::SendTimeoutError};

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
    queue: &mpsc::Sender<Packet>,
    packet: Packet,
    queue_name: &'static str,
    closed_msg: &'static str,
) -> Result<(), TransportError> {
    match queue.send_timeout(packet, SEND_QUEUE_WAIT).await {
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
        let (tx, _rx) = mpsc::channel(2);
        assert!(send_bounded(&tx, packet(1), "q", "closed").await.is_ok());
    }

    /// The regression guard: a burst that briefly fills the queue must still be
    /// delivered once the writer drains it, not rejected outright.
    #[tokio::test]
    async fn absorbs_transient_burst_instead_of_failing() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(packet(1)).await.unwrap();

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
        let (tx, _rx) = mpsc::channel(2);
        tx.send(packet(1)).await.unwrap();
        tx.send(packet(2)).await.unwrap();

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
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        let error = send_bounded(&tx, packet(1), "q", "TCP connection closed")
            .await
            .unwrap_err();

        assert!(
            matches!(error, TransportError::Connection { .. }),
            "unexpected error: {error:?}"
        );
    }
}

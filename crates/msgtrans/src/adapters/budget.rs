//! Byte permits follow Bytes clones, not merely queue slots. This includes
//! payloads retained by an actor or an application after leaving the pipe.
use crate::{Packet, TransportError};
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Debug)]
pub(crate) struct ByteBudget {
    capacity: usize,
    permits: Arc<Semaphore>,
}

struct ChargedBytes<P> {
    bytes: Bytes,
    _permit: P,
}

impl<P> AsRef<[u8]> for ChargedBytes<P> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl ByteBudget {
    pub(crate) fn exhausted(&self) -> TransportError {
        TransportError::resource_error("outbound byte budget", self.capacity, self.capacity)
    }
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            permits: Arc::new(Semaphore::new(capacity)),
        }
    }

    pub(crate) async fn charge(&self, mut packet: Packet) -> Result<Packet, TransportError> {
        let size = packet
            .payload
            .len()
            .saturating_add(packet.ext_header.len())
            .max(1);
        if size > self.capacity {
            return Err(TransportError::resource_error(
                "connection byte budget",
                size,
                self.capacity,
            ));
        }
        let permit = self
            .permits
            .clone()
            .acquire_many_owned(size as u32)
            .await
            .map_err(|_| TransportError::connection_error("byte budget closed", false))?;
        if packet.ext_header.is_empty() {
            // The common case needs only Bytes' owner allocation, not an
            // additional Arc to share a permit with an empty extension.
            packet.payload = Bytes::from_owner(ChargedBytes {
                bytes: packet.payload,
                _permit: permit,
            });
            return Ok(packet);
        }
        let permit = Arc::new(permit);
        packet.payload = Bytes::from_owner(ChargedBytes {
            bytes: packet.payload,
            _permit: permit.clone(),
        });
        packet.ext_header = Bytes::from_owner(ChargedBytes {
            bytes: packet.ext_header,
            _permit: permit,
        });
        Ok(packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn retained_payload_owns_budget_until_last_clone_is_dropped() {
        let budget = ByteBudget::new(8);
        let packet = budget.charge(Packet::one_way(1, vec![0; 8])).await.unwrap();
        let retained = packet.payload().clone();
        drop(packet);
        assert_eq!(budget.permits.available_permits(), 0);
        let pending = budget.charge(Packet::one_way(2, vec![1; 1]));
        tokio::pin!(pending);
        assert!(futures_util::poll!(&mut pending).is_pending());
        drop(retained);
        drop(pending.await.unwrap());
        assert_eq!(budget.permits.available_permits(), 8);
    }
    #[tokio::test]
    async fn oversized_packet_is_rejected_without_waiting() {
        assert!(ByteBudget::new(8)
            .charge(Packet::one_way(1, vec![0; 9]))
            .await
            .is_err());
    }
}

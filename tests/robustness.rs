//! Wire-codec robustness: truncated, oversized, and malformed frames must return
//! an error rather than panic or over-allocate.

use msgtrans::packet::Packet;

#[test]
fn from_bytes_rejects_too_short() {
    assert!(Packet::from_bytes(&[]).is_err());
    assert!(Packet::from_bytes(&[1, 0, 1, 0]).is_err()); // fewer than 16 header bytes
}

#[test]
fn from_bytes_rejects_truncated_payload() {
    let full = Packet::request(1, vec![0u8; 100]).to_bytes();
    let truncated = &full[..full.len() - 50];
    assert!(Packet::from_bytes(truncated).is_err());
}

#[test]
fn from_bytes_rejects_truncated_ext_header() {
    let mut p = Packet::request(1, b"x".to_vec());
    p.set_ext_header(vec![7u8; 32]);
    let full = p.to_bytes();
    // Header declares ext_header_len=32, but we drop the ext body.
    let truncated = &full[..16 + 10];
    assert!(Packet::from_bytes(truncated).is_err());
}

#[test]
fn from_bytes_rejects_bad_version() {
    let mut bytes = Packet::request(1, b"hi".to_vec()).to_bytes().to_vec();
    bytes[0] = 99; // invalid protocol version
    assert!(Packet::from_bytes(&bytes).is_err());
}

#[test]
fn from_bytes_oversized_declared_len_does_not_panic() {
    // Header claims a ~4 GiB payload but the buffer is tiny: must error, not panic/OOM.
    let mut bytes = Packet::request(1, b"hi".to_vec()).to_bytes().to_vec();
    // payload_len lives at bytes 10..14 (big-endian u32).
    bytes[10] = 0xFF;
    bytes[11] = 0xFF;
    bytes[12] = 0xFF;
    bytes[13] = 0xFF;
    assert!(Packet::from_bytes(&bytes).is_err());
}

#[test]
fn from_bytes_after_concatenation_reads_first_packet_only() {
    // Coalesced ("sticky") packets: from_bytes decodes exactly the first packet's
    // declared length and ignores trailing bytes, so a reader can split a stream.
    let a = Packet::request(1, b"first".to_vec()).to_bytes();
    let b = Packet::request(2, b"second".to_vec()).to_bytes();
    let mut joined = a.to_vec();
    joined.extend_from_slice(&b);
    let decoded = Packet::from_bytes(&joined).expect("first packet decodes");
    assert_eq!(decoded.message_id(), 1);
    assert_eq!(decoded.payload, b"first".to_vec());
}

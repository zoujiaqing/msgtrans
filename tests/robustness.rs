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
    let full = Packet::request(1, vec![0u8; 100]).try_encode().unwrap();
    let truncated = &full[..full.len() - 50];
    assert!(Packet::from_bytes(truncated).is_err());
}

#[test]
fn from_bytes_rejects_truncated_ext_header() {
    let mut p = Packet::request(1, b"x".to_vec());
    p.set_ext_header(vec![7u8; 32]);
    let full = p.try_encode().unwrap();
    // Header declares ext_header_len=32, but we drop the ext body.
    let truncated = &full[..16 + 10];
    assert!(Packet::from_bytes(truncated).is_err());
}

#[test]
fn from_bytes_rejects_bad_version() {
    let mut bytes = Packet::request(1, b"hi".to_vec())
        .try_encode()
        .unwrap()
        .to_vec();
    bytes[0] = 99; // invalid protocol version
    assert!(Packet::from_bytes(&bytes).is_err());
}

#[test]
fn from_bytes_oversized_declared_len_does_not_panic() {
    // Header claims a ~4 GiB payload but the buffer is tiny: must error, not panic/OOM.
    let mut bytes = Packet::request(1, b"hi".to_vec())
        .try_encode()
        .unwrap()
        .to_vec();
    // payload_len lives at bytes 10..14 (big-endian u32).
    bytes[10] = 0xFF;
    bytes[11] = 0xFF;
    bytes[12] = 0xFF;
    bytes[13] = 0xFF;
    assert!(Packet::from_bytes(&bytes).is_err());
}

#[test]
fn concatenated_packets_split_via_decode_one_and_fail_decode_exact() {
    // 2.0 contract: from_bytes/decode_exact refuse trailing bytes (a framed
    // message carrying extra bytes is malformed); coalesced ("sticky") byte
    // streams are split with decode_one, which reports the consumed length.
    let a = Packet::request(1, b"first".to_vec()).try_encode().unwrap();
    let b = Packet::request(2, b"second".to_vec()).try_encode().unwrap();
    let mut joined = a.to_vec();
    joined.extend_from_slice(&b);

    assert!(
        Packet::from_bytes(&joined).is_err(),
        "exact decode must reject trailing bytes"
    );

    let (first, used) = Packet::decode_one(&joined)
        .expect("valid stream")
        .expect("complete first packet");
    assert_eq!(first.message_id(), 1);
    assert_eq!(first.payload().as_ref(), b"first");
    let second = Packet::from_bytes(&joined[used..]).expect("second packet is exact");
    assert_eq!(second.message_id(), 2);
    assert_eq!(second.payload().as_ref(), b"second");
}

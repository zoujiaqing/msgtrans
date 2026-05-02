//! Wire-format fixture conformance tests.
//!
//! For every fixture pair in `tests/fixtures/`:
//!   1. `Packet::from_bytes(<bin>)` must yield a packet whose fields match `<json>`.
//!   2. Rebuilding a `Packet` from `<json>` and calling `to_bytes()` must produce
//!      bytes byte-for-byte equal to `<bin>`.
//!
//! Spec: `docs/WIRE_FORMAT.md` §15 (batch 1: 01-08, uncompressed).
//! Regenerate via: `cargo run --example regenerate_fixtures`.

use msgtrans::packet::{CompressionType, Packet, PacketType, ReservedFlags};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize, Debug)]
struct FixtureJson {
    version: u8,
    compression: u8,
    packet_type: u8,
    biz_type: u8,
    message_id: u32,
    ext_header_hex: String,
    payload_hex: String,
    reserved: u16,
}

fn hex_decode(s: &str) -> Vec<u8> {
    assert!(
        s.len() % 2 == 0,
        "hex string length must be even, got {}: {:?}",
        s.len(),
        s
    );
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = decode_nibble(bytes[i]);
        let lo = decode_nibble(bytes[i + 1]);
        out.push((hi << 4) | lo);
        i += 2;
    }
    out
}

fn decode_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => panic!("invalid hex nibble: {:?}", b as char),
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn list_bin_fixtures() -> Vec<PathBuf> {
    let dir = fixtures_dir();
    let mut out: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read fixtures dir {}: {}", dir.display(), e))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("bin"))
        .collect();
    // Stable, platform-independent ordering.
    out.sort();
    out
}

fn build_packet_from_spec(spec: &FixtureJson) -> Packet {
    let mut p = Packet::new(PacketType::from(spec.packet_type), spec.message_id);
    p.set_biz_type(spec.biz_type);
    p.set_compression(CompressionType::from(spec.compression));
    p.set_ext_header(hex_decode(&spec.ext_header_hex));
    p.set_payload(hex_decode(&spec.payload_hex));
    p.header.reserved = ReservedFlags::from_raw(spec.reserved);
    // Spec mandates version=1 for protocol v1; honor whatever the JSON says
    // so that future-version fixtures can be parsed if added.
    p.header.version = spec.version;
    p
}

#[test]
fn fixed_header_is_16_bytes() {
    // Behavioral check: an empty OneWay packet (no ext_header, no payload)
    // must serialize to exactly the fixed-header length (16 bytes).
    let empty = Packet::one_way(0, Vec::<u8>::new());
    assert_eq!(
        empty.to_bytes().len(),
        16,
        "fixed header must serialize to exactly 16 bytes"
    );
}

#[test]
fn fixtures_directory_has_batch_one() {
    let bins = list_bin_fixtures();
    assert!(
        bins.len() >= 8,
        "expected at least 8 batch-1 fixtures in {}, found {}. \
         Run `cargo run --example regenerate_fixtures` to generate them.",
        fixtures_dir().display(),
        bins.len()
    );

    // Spot-check that all 01..=08 prefixes are present.
    let names: Vec<String> = bins
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
        .collect();
    for n in 1..=8u32 {
        let prefix = format!("{:02}_", n);
        assert!(
            names.iter().any(|name| name.starts_with(&prefix)),
            "missing fixture with prefix {:?} in {:?}",
            prefix,
            names
        );
    }
}

#[test]
fn fixtures_round_trip() {
    let bins = list_bin_fixtures();
    assert!(!bins.is_empty(), "no fixtures found");

    for bin_path in bins {
        let json_path = bin_path.with_extension("json");
        let label = bin_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        let bin_bytes = fs::read(&bin_path)
            .unwrap_or_else(|e| panic!("[{}] cannot read {}: {}", label, bin_path.display(), e));
        let json_str = fs::read_to_string(&json_path)
            .unwrap_or_else(|e| panic!("[{}] cannot read {}: {}", label, json_path.display(), e));
        let spec: FixtureJson = serde_json::from_str(&json_str)
            .unwrap_or_else(|e| panic!("[{}] invalid JSON: {}", label, e));

        // ---- Direction 1: parse .bin, verify fields match .json ----
        let parsed = Packet::from_bytes(&bin_bytes)
            .unwrap_or_else(|e| panic!("[{}] from_bytes failed: {:?}", label, e));

        assert_eq!(parsed.header.version, spec.version, "[{}] version", label);
        assert_eq!(
            u8::from(parsed.header.compression),
            spec.compression,
            "[{}] compression",
            label
        );
        assert_eq!(
            u8::from(parsed.header.packet_type),
            spec.packet_type,
            "[{}] packet_type",
            label
        );
        assert_eq!(parsed.header.biz_type, spec.biz_type, "[{}] biz_type", label);
        assert_eq!(
            parsed.header.message_id, spec.message_id,
            "[{}] message_id",
            label
        );
        assert_eq!(
            parsed.header.reserved.raw(),
            spec.reserved,
            "[{}] reserved",
            label
        );

        let expected_ext = hex_decode(&spec.ext_header_hex);
        let expected_payload = hex_decode(&spec.payload_hex);
        assert_eq!(parsed.ext_header, expected_ext, "[{}] ext_header", label);
        assert_eq!(parsed.payload, expected_payload, "[{}] payload", label);

        // Header length fields must match wire reality.
        assert_eq!(
            parsed.header.ext_header_len as usize,
            expected_ext.len(),
            "[{}] ext_header_len header field",
            label
        );
        assert_eq!(
            parsed.header.payload_len as usize,
            expected_payload.len(),
            "[{}] payload_len header field",
            label
        );

        // ---- Direction 2: rebuild from .json, verify bytes match .bin ----
        let rebuilt = build_packet_from_spec(&spec);
        let rebuilt_bytes = rebuilt.to_bytes();
        assert_eq!(
            rebuilt_bytes.as_ref(),
            bin_bytes.as_slice(),
            "[{}] byte-for-byte mismatch (rebuilt {} bytes, on-disk {} bytes)",
            label,
            rebuilt_bytes.len(),
            bin_bytes.len()
        );

        // Total length consistency.
        assert_eq!(
            bin_bytes.len(),
            16 + expected_ext.len() + expected_payload.len(),
            "[{}] total length mismatch",
            label
        );
    }
}

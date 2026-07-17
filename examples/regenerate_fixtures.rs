//! Regenerate the first batch of wire-format fixtures.
//!
//! Output: `tests/fixtures/NN_<name>.bin` + `tests/fixtures/NN_<name>.json`.
//!
//! Run with: `cargo run --example regenerate_fixtures`
//!
//! Spec: see `docs/WIRE_FORMAT.md` §15.
//! Batch 1 (this tool) covers fixtures 01-08, all uncompressed.
//! Batch 2 (compression) is intentionally not generated here.

use msgtrans::packet::{Packet, ReservedFlags};
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize)]
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

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn to_json(packet: &Packet) -> FixtureJson {
    FixtureJson {
        version: packet.header.version,
        compression: u8::from(packet.header.compression),
        packet_type: u8::from(packet.header.packet_type),
        biz_type: packet.header.biz_type,
        message_id: packet.header.message_id,
        ext_header_hex: hex_encode(&packet.ext_header),
        payload_hex: hex_encode(&packet.payload),
        reserved: packet.header.reserved.raw(),
    }
}

fn build_fixtures() -> Vec<(&'static str, Packet)> {
    let mut out: Vec<(&'static str, Packet)> = Vec::new();

    // 01: OneWay, mid=1, biz_type=0, empty ext, empty payload
    out.push(("01_oneway_empty", Packet::one_way(1, Vec::<u8>::new())));

    // 02: OneWay, mid=42, biz_type=5, payload="hello world"
    let mut p = Packet::one_way(42, b"hello world".to_vec());
    p.set_biz_type(5);
    out.push(("02_oneway_text", p));

    // 03: Request, mid=100, biz_type=10, payload="ping"
    let mut p = Packet::request(100, b"ping".to_vec());
    p.set_biz_type(10);
    out.push(("03_request_minimal", p));

    // 04: Response, mid=100 (matches 03), biz_type=10, payload="pong"
    let mut p = Packet::response(100, b"pong".to_vec());
    p.set_biz_type(10);
    out.push(("04_response_matching", p));

    // 05: OneWay, mid=7, ext_header="route:abc", payload="data"
    let mut p = Packet::one_way(7, b"data".to_vec());
    p.set_ext_header(b"route:abc".to_vec());
    out.push(("05_with_ext_header", p));

    // 06: OneWay, mid=0xDEADBEEF, biz_type=255, empty payload
    let mut p = Packet::one_way(0xDEADBEEF_u32, Vec::<u8>::new());
    p.set_biz_type(255);
    out.push(("06_max_biz_type", p));

    // 07: OneWay, mid=200, biz_type=20, payload="urgent", reserved=HIGH_PRIORITY (0x0002)
    let mut p = Packet::one_way(200, b"urgent".to_vec());
    p.set_biz_type(20);
    p.set_priority(true);
    out.push(("07_high_priority_flag", p));

    // 08: OneWay, mid=300, biz_type=30, ext_header="route:msg", payload="payload",
    //     reserved=HAS_ROUTE_TAG (0x0004)
    let mut p = Packet::one_way(300, b"payload".to_vec());
    p.set_biz_type(30);
    p.set_ext_header(b"route:msg".to_vec());
    p.set_route_tag(true);
    out.push(("08_route_tag_flag", p));

    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    fs::create_dir_all(&out_dir)?;

    let fixtures = build_fixtures();

    // Sanity: fixed header is 16 bytes regardless of payload.
    let header_only = Packet::one_way(0, Vec::<u8>::new());
    assert_eq!(
        header_only.to_bytes().len(),
        16,
        "fixed header must serialize to exactly 16 bytes"
    );

    // Sanity: ReservedFlags raw round-trips.
    assert_eq!(ReservedFlags::from_raw(0x0006).raw(), 0x0006);

    println!(
        "Writing {} fixtures to {}",
        fixtures.len(),
        out_dir.display()
    );

    for (name, packet) in &fixtures {
        let bin_path = out_dir.join(format!("{}.bin", name));
        let json_path = out_dir.join(format!("{}.json", name));

        let bin_bytes = packet.to_bytes();
        fs::write(&bin_path, &bin_bytes)?;

        let json = to_json(packet);
        let json_str = serde_json::to_string_pretty(&json)?;
        fs::write(&json_path, format!("{}\n", json_str))?;

        println!(
            "  {} ({} bytes wire, mid={}, biz={}, type={})",
            name,
            bin_bytes.len(),
            packet.header.message_id,
            packet.header.biz_type,
            u8::from(packet.header.packet_type),
        );
    }

    println!("Done.");
    Ok(())
}

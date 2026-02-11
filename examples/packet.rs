/// Packet encapsulation and unpacking verification example
///
/// Demonstrates basic serialization and deserialization functionality of the msgtrans unified packet system
use msgtrans::packet::{Packet, PacketType};

fn main() {
    println!("[START] Packet serialization and deserialization test");

    // Test messages
    let test_message = "Hello, Packet World!";
    let extend_data = "Extended payload for testing serialization and deserialization";

    // Create different types of packets
    let packets = vec![
        ("One-way message", Packet::one_way(101, test_message)),
        ("Request message", Packet::request(102, extend_data)),
        ("Response message", Packet::response(103, "Response test")),
        (
            "Binary data",
            Packet::one_way(105, &[0x00u8, 0x01, 0x02, 0x03, 0xFF, 0xFE][..]),
        ),
    ];

    // Test each packet
    for (name, packet) in packets {
        println!(
            "  {} - type: {:?}, ID: {}, payload: {} bytes",
            name,
            packet.header.packet_type,
            packet.header.message_id,
            packet.payload.len()
        );

        // Test serialization
        let serialized = packet.to_bytes();
        println!("    Serialized: {} bytes", serialized.len());

        // Test deserialization
        match Packet::from_bytes(&serialized) {
            Ok(recovered) => {
                println!(
                    "    Deserialization successful: {} bytes",
                    recovered.payload.len()
                );

                // Verify data consistency
                if packet == recovered {
                    println!("    [SUCCESS] Data consistency check passed");
                } else {
                    println!("    [ERROR] Data consistency check failed");
                }
            }
            Err(e) => {
                println!("    [ERROR] Deserialization failed: {:?}", e);
            }
        }
        println!();
    }

    // Detailed serialization test
    println!("[INFO] Detailed serialization test");

    let test_packet = Packet::one_way(999, test_message);

    println!("  Original packet:");
    println!("    Type: {:?}", test_packet.header.packet_type);
    println!("    Message ID: {}", test_packet.header.message_id);
    println!("    Payload length: {} bytes", test_packet.payload.len());
    if let Some(text) = test_packet.payload_as_string() {
        println!("    Payload content: \"{}\"", text);
    }

    // Serialization
    let bytes = test_packet.to_bytes();
    println!("  After serialization: {} bytes", bytes.len());
    println!(
        "    First 16 bytes (header): {:02X?}",
        &bytes[0..16.min(bytes.len())]
    );

    // Deserialization
    match Packet::from_bytes(&bytes) {
        Ok(recovered_packet) => {
            println!("  Deserialization:");
            println!("    Type: {:?}", recovered_packet.header.packet_type);
            println!("    Message ID: {}", recovered_packet.header.message_id);
            println!(
                "    Payload length: {} bytes",
                recovered_packet.payload.len()
            );
            if let Some(text) = recovered_packet.payload_as_string() {
                println!("    Payload content: \"{}\"", text);
            }

            // Integrity check
            if test_packet == recovered_packet {
                println!("  [SUCCESS] Integrity check passed");
            } else {
                println!("  [ERROR] Integrity check failed");
            }
        }
        Err(e) => {
            println!("  [ERROR] Deserialization failed: {:?}", e);
        }
    }

    println!("[TARGET] Packet test completed");
}

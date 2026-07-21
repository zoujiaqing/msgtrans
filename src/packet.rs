use bytes::{Bytes, BytesMut};
/// Unified architecture packet definition
///
/// Simplified, efficient packet format designed for unified architecture
use std::sync::atomic::{AtomicU32, Ordering};

const MAX_DECOMPRESSED_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;

/// Packet types - simplified to 3 core types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    /// One-way message (no reply required)
    OneWay = 0,
    /// Request (reply required)
    Request = 1,
    /// Response message
    Response = 2,
}

impl PacketType {
    /// Backward compatibility: Data type alias
    pub const Data: PacketType = PacketType::OneWay;
}

impl From<u8> for PacketType {
    fn from(value: u8) -> Self {
        match value {
            0 => PacketType::OneWay,
            1 => PacketType::Request,
            2 => PacketType::Response,
            _ => PacketType::OneWay, // Default value
        }
    }
}

impl From<PacketType> for u8 {
    fn from(packet_type: PacketType) -> Self {
        packet_type as u8
    }
}

/// Compression types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionType {
    None = 0,
    Zstd = 1,
    Zlib = 2,
}

impl From<u8> for CompressionType {
    fn from(value: u8) -> Self {
        match value {
            0 => CompressionType::None,
            1 => CompressionType::Zstd,
            2 => CompressionType::Zlib,
            _ => CompressionType::None,
        }
    }
}

impl From<CompressionType> for u8 {
    fn from(compression: CompressionType) -> Self {
        compression as u8
    }
}

/// How a protocol adapter treats a frame it cannot decode as a msgtrans packet.
///
/// The default is `Lenient`, preserving the historical WebSocket/QUIC behavior
/// of delivering undecodable bytes as a raw one-way message. `Strict` treats
/// such frames as a protocol error and closes the connection (matching how the
/// TCP adapter already handles a malformed first packet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum FramePolicy {
    /// Deliver undecodable bytes as a raw one-way message (default).
    #[default]
    Lenient = 0,
    /// Treat undecodable frames as a protocol error and close the connection.
    Strict = 1,
}

impl From<u8> for FramePolicy {
    fn from(value: u8) -> Self {
        match value {
            1 => FramePolicy::Strict,
            _ => FramePolicy::Lenient,
        }
    }
}

/// Reserved field flags
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedFlags(u16);

impl ReservedFlags {
    /// Create empty flags
    pub fn new() -> Self {
        Self(0)
    }

    /// Set fragmentation flag
    pub fn with_fragmented(mut self, fragmented: bool) -> Self {
        if fragmented {
            self.0 |= 0x0001;
        } else {
            self.0 &= !0x0001;
        }
        self
    }

    /// Check if fragmented
    pub fn is_fragmented(&self) -> bool {
        (self.0 & 0x0001) != 0
    }

    /// Set priority flag
    pub fn with_priority(mut self, high_priority: bool) -> Self {
        if high_priority {
            self.0 |= 0x0002;
        } else {
            self.0 &= !0x0002;
        }
        self
    }

    /// Check if high priority
    pub fn is_high_priority(&self) -> bool {
        (self.0 & 0x0002) != 0
    }

    /// Set route tag
    pub fn with_route_tag(mut self, has_route: bool) -> Self {
        if has_route {
            self.0 |= 0x0004;
        } else {
            self.0 &= !0x0004;
        }
        self
    }

    /// Check if has route tag
    pub fn has_route_tag(&self) -> bool {
        (self.0 & 0x0004) != 0
    }

    /// Get raw value
    pub fn raw(&self) -> u16 {
        self.0
    }

    /// Create from raw value
    pub fn from_raw(value: u16) -> Self {
        Self(value)
    }
}

impl Default for ReservedFlags {
    fn default() -> Self {
        Self::new()
    }
}

/// 16-byte fixed header - optimized field order
#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedHeader {
    /// Protocol version (1 byte)
    pub version: u8,
    /// Compression algorithm (1 byte)
    pub compression: CompressionType,
    /// Packet type (1 byte)
    pub packet_type: PacketType,
    /// Application business type (1 byte) - 0-255, defined by business layer
    pub biz_type: u8,
    /// Message ID (4 bytes)
    pub message_id: u32,
    /// Extended header length (2 bytes)
    pub ext_header_len: u16,
    /// Payload length (4 bytes)
    pub payload_len: u32,
    /// Reserved field (2 bytes) - flags for fragmentation, priority, routing, etc.
    pub reserved: ReservedFlags,
}

impl FixedHeader {
    /// Create new fixed header
    pub fn new(packet_type: PacketType, message_id: u32) -> Self {
        Self {
            version: 1,
            compression: CompressionType::None,
            packet_type,
            biz_type: 0, // Default business type
            message_id,
            ext_header_len: 0,
            payload_len: 0,
            reserved: ReservedFlags::new(),
        }
    }

    /// Serialize to byte array (big endian)
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0] = self.version;
        bytes[1] = u8::from(self.compression);
        bytes[2] = u8::from(self.packet_type);
        bytes[3] = self.biz_type;
        bytes[4..8].copy_from_slice(&self.message_id.to_be_bytes());
        bytes[8..10].copy_from_slice(&self.ext_header_len.to_be_bytes());
        bytes[10..14].copy_from_slice(&self.payload_len.to_be_bytes());
        bytes[14..16].copy_from_slice(&self.reserved.raw().to_be_bytes());
        bytes
    }

    /// Deserialize from byte array (big endian)
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < 16 {
            return Err(PacketError::InvalidHeader("Header too short".to_string()));
        }

        let version = bytes[0];
        if version != 1 {
            return Err(PacketError::UnsupportedVersion(version));
        }

        let compression = CompressionType::from(bytes[1]);
        let packet_type = PacketType::from(bytes[2]);
        let biz_type = bytes[3];

        let message_id = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let ext_header_len = u16::from_be_bytes([bytes[8], bytes[9]]);
        let payload_len = u32::from_be_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
        let reserved = ReservedFlags::from_raw(u16::from_be_bytes([bytes[14], bytes[15]]));

        Ok(Self {
            version,
            compression,
            packet_type,
            biz_type,
            message_id,
            ext_header_len,
            payload_len,
            reserved,
        })
    }
}

/// Message ID manager - thread safe
#[derive(Debug)]
pub struct MessageIdManager {
    counter: AtomicU32,
}

impl MessageIdManager {
    /// Create new ID manager
    pub fn new() -> Self {
        Self {
            counter: AtomicU32::new(1), // Start from 1
        }
    }

    /// Get next ID
    pub fn next_id(&self) -> u32 {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        if id == u32::MAX {
            // Reset to 1 when reaching maximum value
            self.counter.store(1, Ordering::SeqCst);
            1
        } else {
            id
        }
    }

    /// Reset ID counter (for connection rebuilding)
    pub fn reset(&self) {
        self.counter.store(1, Ordering::SeqCst);
    }

    /// Get current ID (without incrementing)
    pub fn current_id(&self) -> u32 {
        self.counter.load(Ordering::SeqCst)
    }
}

impl Default for MessageIdManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Packet structure
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Fixed header
    pub header: FixedHeader,
    /// Extended header (optional)
    pub ext_header: Vec<u8>,
    /// Payload data.
    ///
    /// Backed by [`Bytes`] so decoding can slice the read buffer instead of
    /// copying, and encoding can hand the buffer over without another copy.
    /// The wire format is unchanged. `Bytes` derefs to `&[u8]`, so read-only
    /// use (`&packet.payload`, `.len()`, indexing) is identical to a `Vec<u8>`.
    pub payload: Bytes,
}

impl Packet {
    /// Create new packet
    pub fn new(packet_type: PacketType, message_id: u32) -> Self {
        Self {
            header: FixedHeader::new(packet_type, message_id),
            ext_header: Vec::new(),
            payload: Bytes::new(),
        }
    }

    /// Create one-way message
    pub fn one_way(message_id: u32, payload: impl Into<Bytes>) -> Self {
        let mut packet = Self::new(PacketType::OneWay, message_id);
        packet.set_payload(payload);
        packet
    }

    /// Create request message
    pub fn request(message_id: u32, payload: impl Into<Bytes>) -> Self {
        let mut packet = Self::new(PacketType::Request, message_id);
        packet.set_payload(payload);
        packet
    }

    /// Create response message
    pub fn response(message_id: u32, payload: impl Into<Bytes>) -> Self {
        let mut packet = Self::new(PacketType::Response, message_id);
        packet.set_payload(payload);
        packet
    }

    /// Set payload
    pub fn set_payload(&mut self, payload: impl Into<Bytes>) {
        self.payload = payload.into();
        self.header.payload_len = self.payload.len() as u32;
    }

    /// Set message ID
    pub fn set_message_id(&mut self, message_id: u32) {
        self.header.message_id = message_id;
    }

    /// Set packet type
    pub fn set_packet_type(&mut self, packet_type: PacketType) {
        self.header.packet_type = packet_type;
    }

    /// Set extension header
    pub fn set_ext_header(&mut self, ext_header: impl Into<Vec<u8>>) {
        self.ext_header = ext_header.into();
        self.header.ext_header_len = self.ext_header.len() as u16;
    }

    /// Set compression type
    pub fn set_compression(&mut self, compression: CompressionType) {
        self.header.compression = compression;
    }

    /// Set fragmentation flag
    pub fn set_fragmented(&mut self, fragmented: bool) {
        self.header.reserved = self.header.reserved.with_fragmented(fragmented);
    }

    /// Set priority
    pub fn set_priority(&mut self, high_priority: bool) {
        self.header.reserved = self.header.reserved.with_priority(high_priority);
    }

    /// Set business type
    pub fn set_biz_type(&mut self, biz_type: u8) {
        self.header.biz_type = biz_type;
    }

    /// Get business type
    pub fn biz_type(&self) -> u8 {
        self.header.biz_type
    }

    /// Get compression type
    pub fn compression(&self) -> CompressionType {
        self.header.compression
    }

    /// Check if fragmented
    pub fn is_fragmented(&self) -> bool {
        self.header.reserved.is_fragmented()
    }

    /// Check if high priority
    pub fn is_high_priority(&self) -> bool {
        self.header.reserved.is_high_priority()
    }

    /// Set route tag
    pub fn set_route_tag(&mut self, has_route: bool) {
        self.header.reserved = self.header.reserved.with_route_tag(has_route);
    }

    /// Check if has route tag
    pub fn has_route_tag(&self) -> bool {
        self.header.reserved.has_route_tag()
    }

    /// Compress payload
    pub fn compress_payload(&mut self) -> Result<(), PacketError> {
        let compression = self.header.compression;
        if compression == CompressionType::None {
            return Ok(());
        }

        self.payload = Bytes::from(Self::compress_data(&self.payload, compression)?);
        self.header.payload_len = self.payload.len() as u32;
        Ok(())
    }

    /// Decompress payload
    pub fn decompress_payload(&mut self) -> Result<(), PacketError> {
        let compression = self.header.compression;
        if compression == CompressionType::None {
            return Ok(());
        }

        self.payload = Bytes::from(Self::decompress_data(&self.payload, compression)?);
        self.header.payload_len = self.payload.len() as u32;
        Ok(())
    }

    /// Serialize to Bytes (zero-copy optimized)
    pub fn to_bytes(&self) -> Bytes {
        let total_len = 16 + self.ext_header.len() + self.payload.len();
        let mut buf = BytesMut::with_capacity(total_len);

        // Fixed header
        buf.extend_from_slice(&self.header.to_bytes());

        // Extension header
        if !self.ext_header.is_empty() {
            buf.extend_from_slice(&self.ext_header);
        }

        // Payload
        buf.extend_from_slice(&self.payload);

        buf.freeze()
    }

    /// Serialize to a `Vec<u8>` in a single allocation.
    ///
    /// Equivalent to `to_bytes().to_vec()` but without the intermediate `Bytes`
    /// allocation, for sinks that need an owned `Vec` (e.g. the WebSocket adapter).
    pub fn encode_to_vec(&self) -> Vec<u8> {
        let total_len = 16 + self.ext_header.len() + self.payload.len();
        let mut buf = Vec::with_capacity(total_len);
        buf.extend_from_slice(&self.header.to_bytes());
        if !self.ext_header.is_empty() {
            buf.extend_from_slice(&self.ext_header);
        }
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Deserialize from byte array
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < 16 {
            return Err(PacketError::InvalidPacket("Packet too short".to_string()));
        }

        // Parse fixed header
        let header = FixedHeader::from_bytes(&bytes[0..16])?;

        let mut offset = 16;

        // Parse extension header
        let ext_header = if header.ext_header_len > 0 {
            let end = offset + header.ext_header_len as usize;
            if bytes.len() < end {
                return Err(PacketError::InvalidPacket(
                    "Extended header incomplete".to_string(),
                ));
            }
            let ext_header = bytes[offset..end].to_vec();
            offset = end;
            ext_header
        } else {
            Vec::new()
        };

        // Parse payload
        let payload = if header.payload_len > 0 {
            let end = offset + header.payload_len as usize;
            if bytes.len() < end {
                return Err(PacketError::InvalidPacket("Payload incomplete".to_string()));
            }
            Bytes::copy_from_slice(&bytes[offset..end])
        } else {
            Bytes::new()
        };

        Ok(Self {
            header,
            ext_header,
            payload,
        })
    }

    /// Get packet type
    pub fn packet_type(&self) -> PacketType {
        self.header.packet_type
    }

    /// Get message ID
    pub fn message_id(&self) -> u32 {
        self.header.message_id
    }

    /// Get payload size
    pub fn payload_len(&self) -> usize {
        self.payload.len()
    }

    /// Get total size
    pub fn total_len(&self) -> usize {
        16 + self.ext_header.len() + self.payload.len()
    }

    /// Get string representation of payload (if valid UTF-8)
    pub fn payload_as_string(&self) -> Option<String> {
        String::from_utf8(self.payload.to_vec()).ok()
    }

    /// Compress data
    fn compress_data(data: &[u8], compression: CompressionType) -> Result<Vec<u8>, PacketError> {
        match compression {
            CompressionType::None => Ok(data.to_vec()),
            CompressionType::Zlib => {
                #[cfg(feature = "flate2")]
                {
                    use flate2::{write::ZlibEncoder, Compression};
                    use std::io::Write;

                    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
                    encoder
                        .write_all(data)
                        .map_err(|e| PacketError::CompressionError(e.to_string()))?;
                    encoder
                        .finish()
                        .map_err(|e| PacketError::CompressionError(e.to_string()))
                }
                #[cfg(not(feature = "flate2"))]
                Err(PacketError::UnsupportedCompression(
                    "flate2 feature not enabled".to_string(),
                ))
            }
            CompressionType::Zstd => {
                #[cfg(feature = "zstd")]
                {
                    zstd::bulk::compress(data, 3)
                        .map_err(|e| PacketError::CompressionError(e.to_string()))
                }
                #[cfg(not(feature = "zstd"))]
                Err(PacketError::UnsupportedCompression(
                    "zstd feature not enabled".to_string(),
                ))
            }
        }
    }

    /// Decompress data
    fn decompress_data(data: &[u8], compression: CompressionType) -> Result<Vec<u8>, PacketError> {
        match compression {
            CompressionType::None => Ok(data.to_vec()),
            CompressionType::Zlib => {
                #[cfg(feature = "flate2")]
                {
                    use flate2::read::ZlibDecoder;
                    use std::io::Read;

                    let mut decoder = ZlibDecoder::new(data);
                    let mut result = Vec::new();
                    let limit = (MAX_DECOMPRESSED_PAYLOAD_SIZE + 1) as u64;
                    decoder
                        .take(limit)
                        .read_to_end(&mut result)
                        .map_err(|e| PacketError::CompressionError(e.to_string()))?;
                    if result.len() > MAX_DECOMPRESSED_PAYLOAD_SIZE {
                        return Err(PacketError::CompressionError(format!(
                            "decompressed payload exceeds {} bytes",
                            MAX_DECOMPRESSED_PAYLOAD_SIZE
                        )));
                    }
                    Ok(result)
                }
                #[cfg(not(feature = "flate2"))]
                Err(PacketError::UnsupportedCompression(
                    "flate2 feature not enabled".to_string(),
                ))
            }
            CompressionType::Zstd => {
                #[cfg(feature = "zstd")]
                {
                    zstd::bulk::decompress(data, MAX_DECOMPRESSED_PAYLOAD_SIZE)
                        .map_err(|e| PacketError::CompressionError(e.to_string()))
                }
                #[cfg(not(feature = "zstd"))]
                Err(PacketError::UnsupportedCompression(
                    "zstd feature not enabled".to_string(),
                ))
            }
        }
    }
}

/// Packet error type
#[derive(Debug, thiserror::Error)]
pub enum PacketError {
    #[error("Invalid header: {0}")]
    InvalidHeader(String),

    #[error("Invalid packet: {0}")]
    InvalidPacket(String),

    #[error("Unsupported version: {0}")]
    UnsupportedVersion(u8),

    #[error("Compression error: {0}")]
    CompressionError(String),

    #[error("Unsupported compression: {0}")]
    UnsupportedCompression(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_type_conversion() {
        assert_eq!(u8::from(PacketType::OneWay), 0);
        assert_eq!(u8::from(PacketType::Request), 1);
        assert_eq!(u8::from(PacketType::Response), 2);

        assert_eq!(PacketType::from(0), PacketType::OneWay);
        assert_eq!(PacketType::from(1), PacketType::Request);
        assert_eq!(PacketType::from(2), PacketType::Response);
    }

    #[test]
    fn test_compression_type_conversion() {
        assert_eq!(u8::from(CompressionType::None), 0);
        assert_eq!(u8::from(CompressionType::Zstd), 1);
        assert_eq!(u8::from(CompressionType::Zlib), 2);

        assert_eq!(CompressionType::from(0), CompressionType::None);
        assert_eq!(CompressionType::from(1), CompressionType::Zstd);
        assert_eq!(CompressionType::from(2), CompressionType::Zlib);
    }

    #[test]
    fn test_fixed_header_serialization() {
        let header = FixedHeader {
            version: 1,
            compression: CompressionType::Zstd,
            packet_type: PacketType::Request,
            biz_type: 0,
            message_id: 12345,
            ext_header_len: 8,
            payload_len: 1024,
            reserved: ReservedFlags::new(),
        };

        let bytes = header.to_bytes();
        let recovered = FixedHeader::from_bytes(&bytes).unwrap();

        assert_eq!(header, recovered);
        assert_eq!(bytes.len(), 16);
    }

    #[test]
    fn test_message_id_manager() {
        let manager = MessageIdManager::new();

        assert_eq!(manager.next_id(), 1);
        assert_eq!(manager.next_id(), 2);
        assert_eq!(manager.next_id(), 3);

        manager.reset();
        assert_eq!(manager.next_id(), 1);
    }

    #[test]
    fn test_packet_creation() {
        let mut packet = Packet::one_way(123, b"hello world".to_vec());
        packet.set_compression(CompressionType::Zstd);
        packet.set_fragmented(true);

        assert_eq!(packet.header.packet_type, PacketType::OneWay);
        assert_eq!(packet.header.message_id, 123);
        assert_eq!(packet.payload_len(), 11);
        assert_eq!(packet.header.compression, CompressionType::Zstd);
        assert!(packet.header.reserved.is_fragmented());
    }

    #[test]
    fn encode_to_vec_matches_to_bytes() {
        let mut packet = Packet::request(7, b"payload".to_vec());
        packet.set_ext_header(b"ext");
        assert_eq!(packet.encode_to_vec(), packet.to_bytes().to_vec());
    }

    #[test]
    fn test_packet_serialization() {
        let packet = Packet::request(456, "test message");
        let bytes = packet.to_bytes();
        let recovered = Packet::from_bytes(&bytes).unwrap();

        assert_eq!(packet, recovered);
    }

    #[test]
    fn test_packet_with_ext_header() {
        let mut packet = Packet::response(789, "response data");
        packet.set_ext_header(b"extension");

        let bytes = packet.to_bytes();
        let recovered = Packet::from_bytes(&bytes).unwrap();

        assert_eq!(packet, recovered);
        assert_eq!(recovered.ext_header, b"extension");
    }

    #[test]
    fn test_compression() {
        let original_data = b"Hello, World! This is a test message for compression.".repeat(10);

        // Test no compression
        let compressed = Packet::compress_data(&original_data, CompressionType::None).unwrap();
        let decompressed = Packet::decompress_data(&compressed, CompressionType::None).unwrap();
        assert_eq!(original_data, decompressed);

        // Test Zlib compression (if enabled)
        #[cfg(feature = "flate2")]
        {
            let compressed = Packet::compress_data(&original_data, CompressionType::Zlib).unwrap();
            let decompressed = Packet::decompress_data(&compressed, CompressionType::Zlib).unwrap();
            assert_eq!(original_data, decompressed);
        }

        // Test Zstd compression (if enabled)
        #[cfg(feature = "zstd")]
        {
            let compressed = Packet::compress_data(&original_data, CompressionType::Zstd).unwrap();
            let decompressed = Packet::decompress_data(&compressed, CompressionType::Zstd).unwrap();
            assert_eq!(original_data, decompressed);
        }
    }

    #[cfg(feature = "flate2")]
    #[test]
    fn zlib_decompress_rejects_payload_over_limit() {
        // S3 regression: a small compressed blob that inflates past the 16MB cap
        // must be rejected instead of exhausting memory via unbounded read_to_end.
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let bomb_input = vec![0u8; 17 * 1024 * 1024];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bomb_input).unwrap();
        let compressed = encoder.finish().unwrap();

        assert!(
            Packet::decompress_data(&compressed, CompressionType::Zlib).is_err(),
            "zlib decompression exceeding the cap must be rejected"
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_decompress_rejects_payload_over_limit() {
        // S3 regression for the zstd path (previously capped at 1MB, now 16MB).
        let bomb_input = vec![0u8; 17 * 1024 * 1024];
        let compressed = zstd::bulk::compress(&bomb_input, 3).unwrap();

        assert!(
            Packet::decompress_data(&compressed, CompressionType::Zstd).is_err(),
            "zstd decompression exceeding the cap must be rejected"
        );
    }

    #[test]
    fn test_reserved_flags() {
        let mut flags = ReservedFlags::new();
        assert!(!flags.is_fragmented());
        assert!(!flags.is_high_priority());
        assert!(!flags.has_route_tag());

        flags = flags.with_fragmented(true);
        assert!(flags.is_fragmented());

        flags = flags.with_priority(true);
        assert!(flags.is_high_priority());

        flags = flags.with_route_tag(true);
        assert!(flags.has_route_tag());
    }

    #[test]
    fn test_packet_creation_with_new_fields() {
        let mut packet = Packet::one_way(123, b"hello world".to_vec());
        packet.set_compression(CompressionType::Zstd);
        packet.set_biz_type(42); // Custom business layer type
        packet.set_fragmented(true);
        packet.set_priority(true);
        packet.set_route_tag(true);

        assert_eq!(packet.header.packet_type, PacketType::OneWay);
        assert_eq!(packet.header.message_id, 123);
        assert_eq!(packet.payload_len(), 11);
        assert_eq!(packet.header.compression, CompressionType::Zstd);
        assert_eq!(packet.header.biz_type, 42);
        assert!(packet.header.reserved.is_fragmented());
        assert!(packet.header.reserved.is_high_priority());
        assert!(packet.header.reserved.has_route_tag());
    }

    #[test]
    fn test_packet_serialization_with_new_format() {
        let mut packet = Packet::request(456, "test message");
        packet.set_biz_type(123); // Custom business layer type
        packet.set_compression(CompressionType::Zlib);

        let bytes = packet.to_bytes();
        let recovered = Packet::from_bytes(&bytes).unwrap();

        assert_eq!(packet, recovered);
        assert_eq!(recovered.biz_type(), 123);
        assert_eq!(recovered.compression(), CompressionType::Zlib);
    }

    #[test]
    fn test_new_byte_order_format() {
        let mut packet = Packet::request(0x12345678, "test");
        packet.set_biz_type(255); // Maximum business type value
        packet.set_compression(CompressionType::Zstd);

        let bytes = packet.to_bytes();

        // Verify new field order
        assert_eq!(bytes[0], 1); // version
        assert_eq!(bytes[1], 1); // compression = Zstd
        assert_eq!(bytes[2], 1); // packet_type = Request
        assert_eq!(bytes[3], 255); // biz_type = 255

        // message_id at bytes 4-7 position, big endian
        assert_eq!(bytes[4], 0x12);
        assert_eq!(bytes[5], 0x34);
        assert_eq!(bytes[6], 0x56);
        assert_eq!(bytes[7], 0x78);

        // ext_header_len at bytes 8-9
        assert_eq!(bytes[8], 0x00);
        assert_eq!(bytes[9], 0x00);

        // payload_len at bytes 10-13
        assert_eq!(bytes[10], 0x00);
        assert_eq!(bytes[11], 0x00);
        assert_eq!(bytes[12], 0x00);
        assert_eq!(bytes[13], 0x04); // "test" = 4 bytes
    }

    #[test]
    fn test_big_endian_format() {
        let packet = Packet::request(0x12345678, "test");
        let bytes = packet.to_bytes();

        // Verify big endian format
        // message_id should be at bytes 4-7 position in new field order, big endian
        assert_eq!(bytes[4], 0x12);
        assert_eq!(bytes[5], 0x34);
        assert_eq!(bytes[6], 0x56);
        assert_eq!(bytes[7], 0x78);
    }

    #[test]
    fn test_protocol_format_stability() {
        // Protocol format stability test - ensure cross-version compatibility

        // Create complex packet containing all fields
        let mut packet = Packet::new(PacketType::Request, 0xDEADBEEF);
        packet.set_biz_type(0xFF);
        packet.set_compression(CompressionType::Zlib);
        packet.set_fragmented(true);
        packet.set_priority(true);
        packet.set_route_tag(true);
        packet.set_ext_header(b"complex_ext_header");
        packet.set_payload(b"complex_payload_data_for_testing".to_vec());

        // Packet serialization
        let packet_bytes = packet.to_bytes();

        // Round-trip must reproduce the packet exactly.
        let recovered = Packet::from_bytes(&packet_bytes).unwrap();
        assert_eq!(recovered, packet);
    }
}

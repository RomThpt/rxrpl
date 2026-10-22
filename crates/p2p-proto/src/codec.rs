use std::io::{self, ErrorKind};

use bytes::{BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::message::MessageType;

/// A framed peer protocol message.
#[derive(Debug)]
pub struct PeerMessage {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
}

/// Length-delimited codec for peer protocol messages.
///
/// Wire format (rippled-compatible): either an uncompressed
/// `[4-byte flags+length][2-byte type][payload]` frame or an LZ4-compressed
/// `[4-byte flags+length][2-byte type][4-byte uncompressed length][payload]`
/// frame. The payload length occupies the low 26 bits of the first word.
pub struct PeerCodec;

const UNCOMPRESSED_HEADER_SIZE: usize = 6;
const COMPRESSED_HEADER_SIZE: usize = 10;
const MAX_PAYLOAD_SIZE: usize = 64 * 1024 * 1024; // 64 MiB
const PAYLOAD_SIZE_MASK: u32 = 0x03FF_FFFF;
const COMPRESSED_FLAG: u8 = 0x80;
const COMPRESSION_MASK: u8 = 0xF0;
const LZ4_COMPRESSION: u8 = 0x90;
const COMPRESSED_RESERVED_MASK: u8 = 0x0C;
const UNCOMPRESSED_FLAGS_MASK: u8 = 0xFC;

impl Decoder for PeerCodec {
    type Item = PeerMessage;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<PeerMessage>, Self::Error> {
        if src.len() < UNCOMPRESSED_HEADER_SIZE {
            return Ok(None);
        }

        let first = src[0];
        let compressed = first & COMPRESSED_FLAG != 0;
        let header_size = if compressed {
            if first & COMPRESSED_RESERVED_MASK != 0 {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "compressed frame has reserved flag bits set",
                ));
            }
            if first & COMPRESSION_MASK != LZ4_COMPRESSION {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unsupported peer compression algorithm: 0x{:02X}", first),
                ));
            }
            COMPRESSED_HEADER_SIZE
        } else {
            if first & UNCOMPRESSED_FLAGS_MASK != 0 {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "uncompressed frame has non-zero flag bits",
                ));
            }
            UNCOMPRESSED_HEADER_SIZE
        };

        if src.len() < header_size {
            return Ok(None);
        }

        let length =
            (u32::from_be_bytes([src[0], src[1], src[2], src[3]]) & PAYLOAD_SIZE_MASK) as usize;
        let msg_type_raw = u16::from_be_bytes([src[4], src[5]]) as u32;

        if length > MAX_PAYLOAD_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("payload too large: {length}"),
            ));
        }

        let uncompressed_length = if compressed {
            let size = u32::from_be_bytes([src[6], src[7], src[8], src[9]]) as usize;
            if size == 0 || size > MAX_PAYLOAD_SIZE {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("invalid uncompressed payload size: {size}"),
                ));
            }
            size
        } else {
            length
        };

        if src.len() < header_size + length {
            return Ok(None);
        }

        let frame = src.split_to(header_size + length);
        let wire_payload = &frame[header_size..];

        match MessageType::from_u32(msg_type_raw) {
            Some(msg_type) => {
                let payload = if compressed {
                    let mut output = vec![0; uncompressed_length];
                    let decoded = lz4_flex::block::decompress_into(wire_payload, &mut output)
                        .map_err(|e| {
                            io::Error::new(
                                ErrorKind::InvalidData,
                                format!("invalid LZ4 peer payload: {e}"),
                            )
                        })?;
                    if decoded != uncompressed_length {
                        return Err(io::Error::new(
                            ErrorKind::InvalidData,
                            format!(
                                "LZ4 peer payload length mismatch: decoded {decoded}, expected {uncompressed_length}"
                            ),
                        ));
                    }
                    output
                } else {
                    wire_payload.to_vec()
                };
                Ok(Some(PeerMessage { msg_type, payload }))
            }
            None => {
                // Keep framing in sync for newer message types without accepting
                // an unbounded decompression workload for data we cannot process.
                self.decode(src)
            }
        }
    }
}

impl Encoder<PeerMessage> for PeerCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: PeerMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.put_u32(item.payload.len() as u32);
        dst.put_u16(item.msg_type as u16);
        dst.extend_from_slice(&item.payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let msg = PeerMessage {
            msg_type: MessageType::Ping,
            payload: vec![1, 2, 3, 4],
        };

        let mut codec = PeerCodec;
        let mut buf = BytesMut::new();
        codec.encode(msg, &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.msg_type, MessageType::Ping);
        assert_eq!(decoded.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn partial_read() {
        let mut codec = PeerCodec;
        let mut buf = BytesMut::from(&[0, 0, 0, 3, 0][..]);
        // Only 5 bytes, need 6 for an uncompressed header.
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn compressed_lz4_frame_decodes_exact_wire_vector() {
        // Rippled/go-xrpl raw-LZ4 literal block for `test`:
        // 0x40 (four literals), followed by the four literal bytes. The 10-byte
        // header has LZ4's 0x90 algorithm nibble, a five-byte wire payload,
        // mtPING (3), and a four-byte uncompressed length.
        let wire = [
            0x90, 0x00, 0x00, 0x05, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04, 0x40, b't', b'e', b's',
            b't',
        ];
        let mut codec = PeerCodec;
        let mut buf = BytesMut::from(&wire[..]);

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded.msg_type, MessageType::Ping);
        assert_eq!(decoded.payload, b"test");
        assert!(buf.is_empty());
    }

    #[test]
    fn compressed_header_waits_for_all_ten_bytes() {
        let mut codec = PeerCodec;
        let mut buf = BytesMut::from(&[0x90, 0, 0, 1, 0, 3][..]);

        assert!(codec.decode(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), UNCOMPRESSED_HEADER_SIZE);
    }

    #[test]
    fn compressed_frame_rejects_reserved_bits() {
        let mut codec = PeerCodec;
        let mut buf = BytesMut::from(&[0x94, 0, 0, 1, 0, 3][..]);

        let err = codec.decode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn compressed_frame_rejects_size_mismatch() {
        // The LZ4 block expands to four bytes, while the header falsely claims
        // five. A peer must not be allowed to smuggle a shorter payload through.
        let wire = [
            0x90, 0x00, 0x00, 0x05, 0x00, 0x03, 0x00, 0x00, 0x00, 0x05, 0x40, b't', b'e', b's',
            b't',
        ];
        let mut codec = PeerCodec;
        let mut buf = BytesMut::from(&wire[..]);

        let err = codec.decode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }
}

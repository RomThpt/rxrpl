use bytes::{Buf, BufMut, BytesMut};
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
/// Wire format (rippled-compatible): `[4-byte length BE][2-byte type BE][payload]`
pub struct PeerCodec;

const HEADER_SIZE: usize = 6;
const MAX_PAYLOAD_SIZE: usize = 64 * 1024 * 1024; // 64 MiB

impl Decoder for PeerCodec {
    type Item = PeerMessage;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<PeerMessage>, Self::Error> {
        if src.len() < HEADER_SIZE {
            return Ok(None);
        }

        let length = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
        let msg_type_raw = u16::from_be_bytes([src[4], src[5]]) as u32;

        if length > MAX_PAYLOAD_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("payload too large: {length}"),
            ));
        }

        if src.len() < HEADER_SIZE + length {
            src.reserve(HEADER_SIZE + length - src.len());
            return Ok(None);
        }

        src.advance(HEADER_SIZE);
        let payload = src.split_to(length).to_vec();

        match MessageType::from_u32(msg_type_raw) {
            Some(msg_type) => Ok(Some(PeerMessage { msg_type, payload })),
            None => {
                // Log unknown message types -- could be compressed (0x8000 flag)
                // or newer rippled message types.
                eprintln!(
                    "[CODEC] skipping unknown message type {} (0x{:04X}), {} bytes payload",
                    msg_type_raw, msg_type_raw, payload.len()
                );
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
        // Only 5 bytes, need 6 for header
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }
}

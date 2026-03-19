#![no_main]
use libfuzzer_sys::fuzz_target;

use bytes::BytesMut;
use tokio_util::codec::Decoder;

fuzz_target!(|data: &[u8]| {
    // Fuzz P2P protocol message decoding via the tokio codec
    let mut codec = rxrpl_p2p_proto::codec::PeerCodec;
    let mut buf = BytesMut::from(data);
    let _ = codec.decode(&mut buf);
});

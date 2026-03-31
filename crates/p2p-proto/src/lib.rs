/// XRPL peer protocol protobuf messages.
///
/// Generated from proto/ripple.proto using prost.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/ripple.rs"));
}

pub mod codec;
pub mod message;
pub mod shard_msg;

pub use message::MessageType;

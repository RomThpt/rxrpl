// tonic's generated service returns Result<_, tonic::Status>, whose Err variant
// exceeds the clippy::result_large_err threshold; the shape is fixed by tonic.
#![allow(clippy::result_large_err)]

pub mod convert;
pub mod server;
pub mod service;
pub mod subscription;

pub use server::start_grpc_server;

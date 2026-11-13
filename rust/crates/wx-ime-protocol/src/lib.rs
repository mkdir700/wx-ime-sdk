#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
pub mod clipboard;
pub mod crypto;
pub mod packet;
pub mod pairing;
pub mod transport;

/// Reconstructed proto2 wire messages. Prefer SDK domain types in applications.
pub mod wire {
    include!(concat!(env!("OUT_DIR"), "/wetype.rs"));
}

/// Native ordinary-file transfer messages; distinct from clipboard COS objects.
pub mod flurry {
    include!(concat!(env!("OUT_DIR"), "/flurry.rs"));
}

/// Canonical protobuf descriptors, including the native file-transfer service.
/// Build-time consumers can generate service bindings without a sibling checkout.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/wetype-descriptors.bin"));

pub use error::{Error, Result};
mod error;

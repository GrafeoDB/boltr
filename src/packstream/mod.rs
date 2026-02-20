//! PackStream binary encoding format for the Bolt protocol.
//!
//! PackStream is a binary presentation format for the exchange of richly-typed
//! data. It uses big-endian byte ordering exclusively.

pub mod decode;
pub mod encode;
pub mod marker;

pub use decode::decode_value;
pub use encode::encode_value;

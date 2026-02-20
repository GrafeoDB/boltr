//! Bolt message chunking: 2-byte length-prefixed framing over TCP.

pub mod reader;
pub mod writer;

pub use reader::ChunkReader;
pub use writer::ChunkWriter;

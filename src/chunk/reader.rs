//! Reads chunked messages from an async byte stream.

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::BoltError;

/// Maximum chunk size (2-byte unsigned length = 65535).
const MAX_CHUNK_SIZE: usize = 65535;

/// Default maximum message size: 16 MiB.
const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Reads Bolt-chunked messages from an `AsyncRead` stream.
///
/// Each message consists of one or more chunks (2-byte big-endian length prefix
/// followed by that many data bytes), terminated by a zero-length chunk (0x0000).
pub struct ChunkReader<R> {
    reader: R,
    buf: BytesMut,
    max_message_size: usize,
}

impl<R: AsyncRead + Unpin> ChunkReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buf: BytesMut::with_capacity(MAX_CHUNK_SIZE),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        }
    }

    /// Sets the maximum allowed message size in bytes.
    ///
    /// Messages exceeding this limit will return a protocol error.
    /// Default: 16 MiB.
    pub fn set_max_message_size(&mut self, max_bytes: usize) {
        self.max_message_size = max_bytes;
    }

    /// Reads a complete message (all chunks until the `0x0000` terminator).
    pub async fn read_message(&mut self) -> Result<BytesMut, BoltError> {
        let mut message = BytesMut::new();

        loop {
            // Read 2-byte chunk length.
            let mut header = [0u8; 2];
            self.reader.read_exact(&mut header).await?;
            let chunk_len = u16::from_be_bytes(header) as usize;

            if chunk_len == 0 {
                // End of message.
                break;
            }

            if message.len() + chunk_len > self.max_message_size {
                return Err(BoltError::Protocol(format!(
                    "message size exceeds limit of {} bytes",
                    self.max_message_size
                )));
            }

            // Read chunk data.
            self.buf.resize(chunk_len, 0);
            self.reader.read_exact(&mut self.buf[..chunk_len]).await?;
            message.extend_from_slice(&self.buf[..chunk_len]);
        }

        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[tokio::test]
    async fn read_single_chunk_message() {
        // One chunk of 3 bytes + terminator.
        let data: Vec<u8> = vec![
            0x00, 0x03, // chunk length = 3
            0x01, 0x02, 0x03, // data
            0x00, 0x00, // terminator
        ];
        let mut reader = ChunkReader::new(Cursor::new(data));
        let msg = reader.read_message().await.unwrap();
        assert_eq!(&msg[..], &[0x01, 0x02, 0x03]);
    }

    #[tokio::test]
    async fn read_multi_chunk_message() {
        let data: Vec<u8> = vec![
            0x00, 0x02, 0xAA, 0xBB, // chunk 1: 2 bytes
            0x00, 0x01, 0xCC, // chunk 2: 1 byte
            0x00, 0x00, // terminator
        ];
        let mut reader = ChunkReader::new(Cursor::new(data));
        let msg = reader.read_message().await.unwrap();
        assert_eq!(&msg[..], &[0xAA, 0xBB, 0xCC]);
    }

    #[tokio::test]
    async fn read_empty_message() {
        // Just a terminator (no data chunks).
        let data: Vec<u8> = vec![0x00, 0x00];
        let mut reader = ChunkReader::new(Cursor::new(data));
        let msg = reader.read_message().await.unwrap();
        assert!(msg.is_empty());
    }

    #[tokio::test]
    async fn read_message_exceeds_limit() {
        // A 4-byte chunk, but with a 2-byte limit.
        let data: Vec<u8> = vec![
            0x00, 0x04, // chunk length = 4
            0x01, 0x02, 0x03, 0x04, // data
            0x00, 0x00, // terminator
        ];
        let mut reader = ChunkReader::new(Cursor::new(data));
        reader.set_max_message_size(2);
        let result = reader.read_message().await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds limit"), "unexpected error: {err}");
    }
}

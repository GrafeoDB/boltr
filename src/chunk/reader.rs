//! Reads chunked messages from an async byte stream.

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::BoltError;

/// Default maximum message size: 16 MiB.
pub(crate) const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Reads Bolt-chunked messages from an `AsyncRead` stream.
///
/// Each message consists of one or more chunks (2-byte big-endian length prefix
/// followed by that many data bytes), terminated by a zero-length chunk (0x0000).
///
/// Memory use is bounded by the configured maximum message size: the reader
/// holds no buffer between messages and checks every chunk header against the
/// limit before reading the chunk.
pub struct ChunkReader<R> {
    reader: R,
    max_message_size: usize,
}

impl<R: AsyncRead + Unpin> ChunkReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
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

    /// The underlying reader.
    pub(crate) fn get_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Reads a complete message (all chunks until the `0x0000` terminator).
    ///
    /// A bare `0x0000` (no data chunks) yields an empty message: Bolt uses it
    /// as a NOOP keep-alive between messages, and callers should skip it.
    ///
    /// Errors with [`BoltError::Io`] when the stream ends (including in the
    /// middle of a chunk or before the terminator) and with
    /// [`BoltError::Protocol`] when the message would exceed the maximum
    /// message size. After an error the stream position is undefined and the
    /// connection should be closed.
    pub async fn read_message(&mut self) -> Result<BytesMut, BoltError> {
        let mut message = BytesMut::new();

        loop {
            // Read 2-byte chunk length.
            let mut header = [0u8; 2];
            self.reader.read_exact(&mut header).await?;
            let chunk_len = usize::from(u16::from_be_bytes(header));

            if chunk_len == 0 {
                // End of message.
                break;
            }

            if message.len().saturating_add(chunk_len) > self.max_message_size {
                return Err(BoltError::Protocol(format!(
                    "message size exceeds limit of {} bytes",
                    self.max_message_size
                )));
            }

            // Read chunk data straight into the message buffer.
            let start = message.len();
            message.resize(start + chunk_len, 0);
            self.reader.read_exact(&mut message[start..]).await?;
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

    #[tokio::test]
    async fn limit_is_inclusive_across_chunks() {
        // Two chunks totalling exactly the limit are accepted; one more byte is not.
        let data: Vec<u8> = vec![0x00, 0x02, 0x01, 0x02, 0x00, 0x02, 0x03, 0x04, 0x00, 0x00];
        let mut reader = ChunkReader::new(Cursor::new(data));
        reader.set_max_message_size(4);
        assert_eq!(&reader.read_message().await.unwrap()[..], &[1, 2, 3, 4]);

        let data: Vec<u8> = vec![
            0x00, 0x02, 0x01, 0x02, 0x00, 0x03, 0x03, 0x04, 0x05, 0x00, 0x00,
        ];
        let mut reader = ChunkReader::new(Cursor::new(data));
        reader.set_max_message_size(4);
        assert!(matches!(
            reader.read_message().await,
            Err(BoltError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn oversized_chunk_header_is_rejected_before_reading_data() {
        // Header announces 65535 bytes but only 3 follow: the size check must
        // fire before any attempt to read (or buffer) the chunk body.
        let data: Vec<u8> = vec![0xFF, 0xFF, 0x01, 0x02, 0x03];
        let mut reader = ChunkReader::new(Cursor::new(data));
        reader.set_max_message_size(1024);
        assert!(matches!(
            reader.read_message().await,
            Err(BoltError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn max_size_chunk_is_read() {
        let mut data = vec![0xFF, 0xFF];
        data.extend((0..65535u32).map(|i| (i % 251) as u8));
        data.extend_from_slice(&[0x00, 0x01, 0xAB, 0x00, 0x00]);
        let mut reader = ChunkReader::new(Cursor::new(data));
        let msg = reader.read_message().await.unwrap();
        assert_eq!(msg.len(), 65536);
        assert_eq!(msg[65534], (65534 % 251) as u8);
        assert_eq!(msg[65535], 0xAB);
    }

    #[tokio::test]
    async fn back_to_back_messages_and_noops() {
        let data: Vec<u8> = vec![
            0x00, 0x00, // NOOP
            0x00, 0x01, 0x11, 0x00, 0x00, // message 1
            0x00, 0x00, // NOOP
            0x00, 0x00, // NOOP
            0x00, 0x02, 0x22, 0x33, 0x00, 0x00, // message 2
        ];
        let mut reader = ChunkReader::new(Cursor::new(data));
        assert!(reader.read_message().await.unwrap().is_empty());
        assert_eq!(&reader.read_message().await.unwrap()[..], &[0x11]);
        assert!(reader.read_message().await.unwrap().is_empty());
        assert!(reader.read_message().await.unwrap().is_empty());
        assert_eq!(&reader.read_message().await.unwrap()[..], &[0x22, 0x33]);
        // Clean EOF afterwards is an I/O error, not a hang or panic.
        assert!(matches!(reader.read_message().await, Err(BoltError::Io(_))));
    }

    #[tokio::test]
    async fn eof_anywhere_is_an_io_error() {
        let cases: &[&[u8]] = &[
            &[],                       // before the header
            &[0x00],                   // inside the header
            &[0x00, 0x04, 0x01, 0x02], // inside the chunk body
            &[0x00, 0x01, 0x01],       // missing terminator
            &[0x00, 0x01, 0x01, 0x00], // half a terminator
        ];
        for bytes in cases {
            let mut reader = ChunkReader::new(Cursor::new(bytes.to_vec()));
            assert!(
                matches!(reader.read_message().await, Err(BoltError::Io(_))),
                "{bytes:02X?}"
            );
        }
    }
}

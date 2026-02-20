//! Writes chunked messages to an async byte stream.

use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::error::BoltError;

/// Maximum chunk size (2-byte unsigned length = 65535).
const MAX_CHUNK_SIZE: usize = 65535;

/// Writes Bolt-chunked messages to an `AsyncWrite` stream.
pub struct ChunkWriter<W> {
    writer: W,
    max_chunk_size: usize,
}

impl<W: AsyncWrite + Unpin> ChunkWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            max_chunk_size: MAX_CHUNK_SIZE,
        }
    }

    /// Writes a complete message, splitting into chunks if needed,
    /// and appends the `0x0000` terminator.
    pub async fn write_message(&mut self, data: &[u8]) -> Result<(), BoltError> {
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + self.max_chunk_size).min(data.len());
            let chunk = &data[offset..end];
            let len = chunk.len() as u16;

            // Write 2-byte length header + chunk data.
            self.writer.write_all(&len.to_be_bytes()).await?;
            self.writer.write_all(chunk).await?;
            offset = end;
        }

        // Write terminator.
        self.writer.write_all(&[0x00, 0x00]).await?;
        Ok(())
    }

    /// Flushes the underlying writer.
    pub async fn flush(&mut self) -> Result<(), BoltError> {
        self.writer.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_small_message() {
        let mut output = Vec::new();
        let mut writer = ChunkWriter::new(&mut output);
        writer.write_message(&[0x01, 0x02, 0x03]).await.unwrap();

        assert_eq!(
            output,
            vec![
                0x00, 0x03, // length
                0x01, 0x02, 0x03, // data
                0x00, 0x00, // terminator
            ]
        );
    }

    #[tokio::test]
    async fn write_empty_message() {
        let mut output = Vec::new();
        let mut writer = ChunkWriter::new(&mut output);
        writer.write_message(&[]).await.unwrap();
        // Just the terminator.
        assert_eq!(output, vec![0x00, 0x00]);
    }
}

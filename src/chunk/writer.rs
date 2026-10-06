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
    ///
    /// The framed message is assembled in memory and handed to the underlying
    /// writer in a single `write_all`, so an unbuffered TCP or TLS stream sees
    /// one write (one TLS record) per message instead of one per header.
    pub async fn write_message(&mut self, data: &[u8]) -> Result<(), BoltError> {
        let chunk_count = data.len().div_ceil(self.max_chunk_size);
        let mut framed = Vec::with_capacity(data.len() + 2 * chunk_count + 2);
        for chunk in data.chunks(self.max_chunk_size) {
            // `max_chunk_size` is 65535, so every chunk length fits in a u16.
            let len = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
            framed.extend_from_slice(&len.to_be_bytes());
            framed.extend_from_slice(chunk);
        }

        // Terminator.
        framed.extend_from_slice(&[0x00, 0x00]);
        self.writer.write_all(&framed).await?;
        Ok(())
    }

    /// Flushes the underlying writer.
    pub async fn flush(&mut self) -> Result<(), BoltError> {
        self.writer.flush().await?;
        Ok(())
    }

    /// The underlying writer.
    pub(crate) fn get_mut(&mut self) -> &mut W {
        &mut self.writer
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

    /// Splits `output` into (chunk lengths, payload) and checks the terminator.
    fn parse_frames(output: &[u8]) -> (Vec<usize>, Vec<u8>) {
        let mut lengths = Vec::new();
        let mut payload = Vec::new();
        let mut pos = 0;
        loop {
            let len = usize::from(u16::from_be_bytes([output[pos], output[pos + 1]]));
            pos += 2;
            if len == 0 {
                break;
            }
            lengths.push(len);
            payload.extend_from_slice(&output[pos..pos + len]);
            pos += len;
        }
        assert_eq!(pos, output.len(), "trailing bytes after terminator");
        (lengths, payload)
    }

    #[tokio::test]
    async fn chunk_boundaries_around_65535() {
        for (size, expected) in [
            (1usize, vec![1usize]),
            (65534, vec![65534]),
            (65535, vec![65535]),
            (65536, vec![65535, 1]),
            (131_070, vec![65535, 65535]),
            (131_071, vec![65535, 65535, 1]),
        ] {
            let data: Vec<u8> = (0..size).map(|i| (i % 253) as u8).collect();
            let mut output = Vec::new();
            ChunkWriter::new(&mut output)
                .write_message(&data)
                .await
                .unwrap();
            let (lengths, payload) = parse_frames(&output);
            assert_eq!(lengths, expected, "size {size}");
            assert_eq!(payload, data, "size {size}");
        }
    }

    #[tokio::test]
    async fn writer_output_round_trips_through_reader() {
        use crate::chunk::ChunkReader;

        let messages: Vec<Vec<u8>> = vec![vec![], vec![1, 2, 3], vec![7; 200_000], vec![9]];
        let mut output = Vec::new();
        let mut writer = ChunkWriter::new(&mut output);
        for message in &messages {
            writer.write_message(message).await.unwrap();
        }
        writer.flush().await.unwrap();

        let mut reader = ChunkReader::new(std::io::Cursor::new(output));
        for message in &messages {
            assert_eq!(&reader.read_message().await.unwrap()[..], &message[..]);
        }
    }
}

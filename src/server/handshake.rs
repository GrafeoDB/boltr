//! Bolt handshake: magic preamble and version negotiation.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::BoltError;
use crate::version::{self, BOLT_MAGIC};

/// Performs the server-side Bolt handshake on a TCP stream.
///
/// 1. Reads 4 bytes of magic preamble (`60 60 B0 17`).
/// 2. Reads 16 bytes (4 version proposals).
/// 3. Negotiates the best matching version.
/// 4. Sends back the matched version (or `00 00 00 00` on failure).
///
/// Returns the negotiated `(major, minor)` version on success.
pub async fn server_handshake<S>(stream: &mut S) -> Result<(u8, u8), BoltError>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // 1. Read magic preamble.
    let mut magic = [0u8; 4];
    stream.read_exact(&mut magic).await?;
    if magic != BOLT_MAGIC {
        return Err(BoltError::Protocol(format!(
            "invalid magic preamble: {:02X?}",
            magic
        )));
    }

    // 2. Read version proposals.
    let mut proposals = [0u8; 16];
    stream.read_exact(&mut proposals).await?;

    // 3. Negotiate.
    match version::negotiate_version(&proposals) {
        Some((major, minor)) => {
            let response = version::encode_version(major, minor);
            stream.write_all(&response).await?;
            stream.flush().await?;
            Ok((major, minor))
        }
        None => {
            stream.write_all(&version::NO_VERSION).await?;
            stream.flush().await?;
            Err(BoltError::Protocol("no compatible Bolt version".into()))
        }
    }
}

/// Performs the client-side Bolt handshake.
///
/// Sends magic + version proposals, reads the negotiated version.
///
/// Fails if the server rejects every proposal, or answers with a version
/// that was not proposed (which is what a non-Bolt server, for example an
/// HTTP server, appears to do).
pub async fn client_handshake<S>(
    stream: &mut S,
    proposals: &[u8; 16],
) -> Result<(u8, u8), BoltError>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // Send magic + proposals in one write.
    let mut request = [0u8; 20];
    request[..4].copy_from_slice(&BOLT_MAGIC);
    request[4..].copy_from_slice(proposals);
    stream.write_all(&request).await?;
    stream.flush().await?;

    // Read response.
    let mut response = [0u8; 4];
    stream.read_exact(&mut response).await?;

    let major = response[3];
    let minor = response[2];

    if major == 0 && minor == 0 {
        return Err(BoltError::Protocol(
            "server rejected all proposed versions".into(),
        ));
    }

    if !version::proposals_cover(proposals, major, minor) {
        return Err(BoltError::Protocol(format!(
            "server selected Bolt version {major}.{minor}, which was not proposed \
             (response {response:02X?}); is this a Bolt server?"
        )));
    }

    Ok((major, minor))
}

/// Builds the default version proposal bytes for a BoltR client.
pub fn default_client_proposals() -> [u8; 16] {
    let mut proposals = [0u8; 16];
    // Slot 0: 5.4 with range 3 (covers 5.4, 5.3, 5.2, 5.1)
    proposals[1] = 3; // range
    proposals[2] = 4; // minor
    proposals[3] = 5; // major
    // Slots 1-3: empty (zeros)
    proposals
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    #[tokio::test]
    async fn handshake_success() {
        let (mut client, mut server) = duplex(256);

        let server_task = tokio::spawn(async move { server_handshake(&mut server).await });

        let client_task = tokio::spawn(async move {
            let proposals = default_client_proposals();
            client_handshake(&mut client, &proposals).await
        });

        let server_version = server_task.await.unwrap().unwrap();
        let client_version = client_task.await.unwrap().unwrap();

        assert_eq!(server_version, (5, 4));
        assert_eq!(client_version, (5, 4));
    }

    /// Feeds `input` to `server_handshake` and returns its result and the
    /// bytes it wrote back.
    async fn server_handshake_with(input: &[u8]) -> (Result<(u8, u8), BoltError>, Vec<u8>) {
        let (mut client, mut server) = duplex(256);
        client.write_all(input).await.unwrap();
        client.shutdown().await.unwrap();
        let result = server_handshake(&mut server).await;
        drop(server);
        let mut written = Vec::new();
        client.read_to_end(&mut written).await.unwrap();
        (result, written)
    }

    #[tokio::test]
    async fn server_rejects_bad_magic_without_answering() {
        let mut input = b"GET / HTTP/1.1".to_vec();
        input.resize(20, 0);
        let (result, written) = server_handshake_with(&input).await;
        assert!(result.unwrap_err().to_string().contains("magic"));
        assert!(written.is_empty());
    }

    #[tokio::test]
    async fn server_handles_truncated_handshakes() {
        let mut full = BOLT_MAGIC.to_vec();
        full.extend_from_slice(&default_client_proposals());
        for len in 0..full.len() {
            let (result, written) = server_handshake_with(&full[..len]).await;
            assert!(
                matches!(result, Err(BoltError::Io(_))),
                "len {len}: {result:?}"
            );
            assert!(written.is_empty(), "len {len}");
        }
    }

    #[tokio::test]
    async fn server_answers_garbage_versions_with_no_version() {
        let mut input = BOLT_MAGIC.to_vec();
        input.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF].repeat(4));
        let (result, written) = server_handshake_with(&input).await;
        assert!(result.is_err());
        assert_eq!(written, version::NO_VERSION);
    }

    #[tokio::test]
    async fn server_picks_version_from_later_slot() {
        let mut input = BOLT_MAGIC.to_vec();
        // 4.4, 3.0, 5.2, empty.
        input.extend_from_slice(&[0, 0, 4, 4, 0, 0, 0, 3, 0, 0, 2, 5, 0, 0, 0, 0]);
        let (result, written) = server_handshake_with(&input).await;
        assert_eq!(result.unwrap(), (5, 2));
        assert_eq!(written, version::encode_version(5, 2));
    }

    /// Regression: the client accepted any non-zero answer as the negotiated
    /// version, so connecting to a non-Bolt port "succeeded" and failed later
    /// with confusing decode errors.
    #[tokio::test]
    async fn client_rejects_unproposed_versions() {
        for response in [*b"HTTP", [0, 0, 0, 4], [0, 0, 5, 5], [0, 0, 0, 5]] {
            let (mut client, mut server) = duplex(256);
            let server_task = tokio::spawn(async move {
                let mut request = [0u8; 20];
                server.read_exact(&mut request).await.unwrap();
                server.write_all(&response).await.unwrap();
                server
            });
            let result = client_handshake(&mut client, &default_client_proposals()).await;
            assert!(result.is_err(), "{response:02X?} accepted: {result:?}");
            drop(server_task.await.unwrap());
        }
    }

    #[tokio::test]
    async fn client_accepts_every_proposed_version() {
        for minor in 1..=4u8 {
            let (mut client, mut server) = duplex(256);
            let server_task = tokio::spawn(async move {
                let mut request = [0u8; 20];
                server.read_exact(&mut request).await.unwrap();
                server
                    .write_all(&version::encode_version(5, minor))
                    .await
                    .unwrap();
                server
            });
            let result = client_handshake(&mut client, &default_client_proposals()).await;
            assert_eq!(result.unwrap(), (5, minor));
            drop(server_task.await.unwrap());
        }
    }

    #[tokio::test]
    async fn handshake_no_match() {
        let (mut client, mut server) = duplex(256);

        let server_task = tokio::spawn(async move { server_handshake(&mut server).await });

        let client_task = tokio::spawn(async move {
            // Propose only Bolt 4.4 (not supported).
            let mut proposals = [0u8; 16];
            proposals[2] = 4;
            proposals[3] = 4;
            client_handshake(&mut client, &proposals).await
        });

        let server_result = server_task.await.unwrap();
        let client_result = client_task.await.unwrap();

        assert!(server_result.is_err());
        assert!(client_result.is_err());
    }
}

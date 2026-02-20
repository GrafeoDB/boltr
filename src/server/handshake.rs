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
pub async fn client_handshake<S>(
    stream: &mut S,
    proposals: &[u8; 16],
) -> Result<(u8, u8), BoltError>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    // Send magic + proposals.
    stream.write_all(&BOLT_MAGIC).await?;
    stream.write_all(proposals).await?;
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
    use tokio::io::duplex;

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

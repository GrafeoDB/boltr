//! Bolt over TLS end to end: a server configured with a certificate
//! generated in the test, and a rustls client that trusts only that
//! certificate.

#![cfg(feature = "tls")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::BytesMut;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use boltr::chunk::{ChunkReader, ChunkWriter};
use boltr::error::BoltError;
use boltr::message::decode::decode_server_message;
use boltr::message::encode::encode_client_message;
use boltr::message::{ClientMessage, ServerMessage};
use boltr::server::handshake::{client_handshake, default_client_proposals};
use boltr::server::{
    BoltBackend, BoltRecord, BoltServer, ResultMetadata, ResultStream, SessionConfig,
    SessionHandle, SessionProperty, TlsConfig, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};
use rustls_pki_types::{CertificateDer, ServerName};

/// Answers every statement with the records `[0]`, `[1]`, `[2]`.
struct CountingBackend;

#[async_trait::async_trait]
impl BoltBackend for CountingBackend {
    async fn create_session(&self, _config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        Ok(SessionHandle("tls".into()))
    }

    async fn close_session(&self, _session: &SessionHandle) -> Result<(), BoltError> {
        Ok(())
    }

    async fn configure_session(
        &self,
        _session: &SessionHandle,
        _property: SessionProperty,
    ) -> Result<(), BoltError> {
        Ok(())
    }

    async fn reset_session(&self, _session: &SessionHandle) -> Result<(), BoltError> {
        Ok(())
    }

    async fn execute(
        &self,
        _session: &SessionHandle,
        _query: &str,
        _parameters: &HashMap<String, BoltValue>,
        _extra: &BoltDict,
        _transaction: Option<&TransactionHandle>,
    ) -> Result<ResultStream, BoltError> {
        Ok(ResultStream {
            metadata: ResultMetadata {
                columns: vec!["i".into()],
                extra: BoltDict::new(),
            },
            records: (0..3)
                .map(|i| BoltRecord {
                    values: vec![BoltValue::Integer(i)],
                })
                .collect(),
            summary: BoltDict::new(),
        })
    }

    async fn begin_transaction(
        &self,
        _session: &SessionHandle,
        _extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError> {
        Ok(TransactionHandle("tx".into()))
    }

    async fn commit(
        &self,
        _session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::new())
    }

    async fn rollback(
        &self,
        _session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        Ok(())
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::new())
    }
}

/// Starts a TLS server with a fresh self-signed certificate for
/// `localhost` and returns its address and the certificate.
async fn start_tls_server() -> (SocketAddr, CertificateDer<'static>) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let tls = TlsConfig::from_pem(
        certified.cert.pem().as_bytes(),
        certified.signing_key.serialize_pem().as_bytes(),
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = BoltServer::builder(CountingBackend).tls(tls);
    tokio::spawn(server.serve_listener(listener));
    (addr, certified.cert.der().clone())
}

fn connector(trusted: CertificateDer<'static>) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots.add(trusted).unwrap();
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, msg: &ClientMessage) {
    let mut payload = BytesMut::new();
    encode_client_message(&mut payload, msg);
    let mut writer = ChunkWriter::new(stream);
    writer.write_message(&payload).await.unwrap();
    writer.flush().await.unwrap();
}

async fn recv<S: AsyncRead + Unpin>(stream: &mut S) -> ServerMessage {
    let data = ChunkReader::new(stream).read_message().await.unwrap();
    decode_server_message(&data).unwrap()
}

#[tokio::test]
async fn full_session_over_tls() {
    let (addr, cert) = start_tls_server().await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let mut tls = connector(cert)
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();

    let version = client_handshake(&mut tls, &default_client_proposals())
        .await
        .unwrap();
    assert_eq!(version, (5, 4));

    for msg in [
        ClientMessage::Hello {
            extra: BoltDict::new(),
        },
        ClientMessage::Logon {
            auth: BoltDict::from([("scheme".to_string(), BoltValue::String("none".into()))]),
        },
        ClientMessage::Run {
            query: "RETURN 1".into(),
            parameters: BoltDict::new(),
            extra: BoltDict::new(),
        },
    ] {
        send(&mut tls, &msg).await;
        assert!(matches!(
            recv(&mut tls).await,
            ServerMessage::Success { .. }
        ));
    }

    send(&mut tls, &ClientMessage::pull_all()).await;
    let mut values = Vec::new();
    loop {
        match recv(&mut tls).await {
            ServerMessage::Record { data } => values.push(data[0].as_int().unwrap()),
            ServerMessage::Success { metadata } => {
                assert_eq!(metadata.get("has_more"), Some(&BoltValue::Boolean(false)));
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(values, vec![0, 1, 2]);

    send(&mut tls, &ClientMessage::Goodbye).await;
    // The server ends the TLS session cleanly (close_notify, then EOF).
    assert!(ChunkReader::new(&mut tls).read_message().await.is_err());
}

#[tokio::test]
async fn plaintext_client_is_rejected_by_a_tls_server() {
    let (addr, _cert) = start_tls_server().await;
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    // A Bolt handshake without TLS: the server cannot complete a TLS
    // handshake from it and closes the connection.
    let mut request = vec![0x60, 0x60, 0xB0, 0x17];
    request.extend_from_slice(&default_client_proposals());
    tcp.write_all(&request).await.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        ChunkReader::new(&mut tcp).read_message(),
    )
    .await
    .expect("server must close the connection");
    assert!(result.is_err());
}

#[tokio::test]
async fn untrusted_certificate_is_refused_by_the_client() {
    let (addr, _cert) = start_tls_server().await;
    // Trust a different self-signed certificate.
    let other = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let tcp = TcpStream::connect(addr).await.unwrap();
    let result = connector(other.cert.der().clone())
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await;
    assert!(result.is_err());
}

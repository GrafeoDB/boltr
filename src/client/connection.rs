//! Low-level Bolt connection: TCP connect, handshake, message I/O.

use std::collections::HashMap;
use std::net::SocketAddr;

use bytes::BytesMut;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::TcpStream;

use crate::chunk::reader::ChunkReader;
use crate::chunk::writer::ChunkWriter;
use crate::error::BoltError;
use crate::message::decode::decode_server_message;
use crate::message::encode::encode_client_message;
use crate::message::request::ClientMessage;
use crate::message::response::ServerMessage;
use crate::server::handshake::{client_handshake, default_client_proposals};
use crate::types::{BoltDict, BoltValue};

/// A low-level Bolt connection that handles handshake and message framing.
pub struct BoltConnection {
    reader: ChunkReader<ReadHalf<TcpStream>>,
    writer: ChunkWriter<WriteHalf<TcpStream>>,
    version: (u8, u8),
}

impl BoltConnection {
    /// Connects to a Bolt server, performs the handshake, and returns
    /// a connection ready for HELLO/LOGON.
    pub async fn connect(addr: SocketAddr) -> Result<Self, BoltError> {
        let mut stream = TcpStream::connect(addr).await?;

        let proposals = default_client_proposals();
        let version = client_handshake(&mut stream, &proposals).await?;

        let (rh, wh) = tokio::io::split(stream);
        Ok(Self {
            reader: ChunkReader::new(rh),
            writer: ChunkWriter::new(wh),
            version,
        })
    }

    /// Returns the negotiated Bolt version.
    pub fn version(&self) -> (u8, u8) {
        self.version
    }

    /// Sends a client message.
    pub async fn send(&mut self, msg: &ClientMessage) -> Result<(), BoltError> {
        let mut buf = BytesMut::new();
        encode_client_message(&mut buf, msg);
        self.writer.write_message(&buf).await
    }

    /// Receives a server message.
    pub async fn recv(&mut self) -> Result<ServerMessage, BoltError> {
        let data = self.reader.read_message().await?;
        decode_server_message(&data)
    }

    /// Sends HELLO and expects SUCCESS.
    pub async fn hello(
        &mut self,
        extra: BoltDict,
    ) -> Result<BoltDict, BoltError> {
        self.send(&ClientMessage::Hello { extra }).await?;
        match self.recv().await? {
            ServerMessage::Success { metadata } => Ok(metadata),
            ServerMessage::Failure { metadata } => Err(BoltError::Authentication(
                metadata
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("HELLO failed")
                    .to_string(),
            )),
            other => Err(BoltError::Protocol(format!(
                "expected SUCCESS after HELLO, got {other:?}"
            ))),
        }
    }

    /// Sends LOGON with auth credentials and expects SUCCESS.
    pub async fn logon(
        &mut self,
        scheme: &str,
        principal: Option<&str>,
        credentials: Option<&str>,
    ) -> Result<(), BoltError> {
        let mut auth = HashMap::new();
        auth.insert("scheme".to_string(), BoltValue::String(scheme.to_string()));
        if let Some(p) = principal {
            auth.insert("principal".to_string(), BoltValue::String(p.to_string()));
        }
        if let Some(c) = credentials {
            auth.insert(
                "credentials".to_string(),
                BoltValue::String(c.to_string()),
            );
        }

        self.send(&ClientMessage::Logon { auth }).await?;
        match self.recv().await? {
            ServerMessage::Success { .. } => Ok(()),
            ServerMessage::Failure { metadata } => Err(BoltError::Authentication(
                metadata
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("LOGON failed")
                    .to_string(),
            )),
            other => Err(BoltError::Protocol(format!(
                "expected SUCCESS after LOGON, got {other:?}"
            ))),
        }
    }

    /// Sends GOODBYE. Does not wait for a response (server closes connection).
    pub async fn goodbye(&mut self) -> Result<(), BoltError> {
        self.send(&ClientMessage::Goodbye).await
    }

    /// Sends RUN and expects SUCCESS with result metadata.
    pub async fn run(
        &mut self,
        query: &str,
        parameters: HashMap<String, BoltValue>,
        extra: BoltDict,
    ) -> Result<BoltDict, BoltError> {
        self.send(&ClientMessage::Run {
            query: query.to_string(),
            parameters,
            extra,
        })
        .await?;
        match self.recv().await? {
            ServerMessage::Success { metadata } => Ok(metadata),
            ServerMessage::Failure { metadata } => Err(BoltError::Query {
                code: metadata
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                message: metadata
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("query failed")
                    .to_string(),
            }),
            other => Err(BoltError::Protocol(format!(
                "expected SUCCESS after RUN, got {other:?}"
            ))),
        }
    }

    /// Sends PULL and collects all records until SUCCESS summary.
    pub async fn pull_all(&mut self) -> Result<(Vec<Vec<BoltValue>>, BoltDict), BoltError> {
        self.send(&ClientMessage::pull_all()).await?;

        let mut records = Vec::new();
        loop {
            match self.recv().await? {
                ServerMessage::Record { data } => {
                    records.push(data);
                }
                ServerMessage::Success { metadata } => {
                    return Ok((records, metadata));
                }
                ServerMessage::Failure { metadata } => {
                    return Err(BoltError::Query {
                        code: metadata
                            .get("code")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        message: metadata
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("pull failed")
                            .to_string(),
                    });
                }
                other => {
                    return Err(BoltError::Protocol(format!(
                        "unexpected message during PULL: {other:?}"
                    )));
                }
            }
        }
    }

    /// Sends BEGIN and expects SUCCESS.
    pub async fn begin(&mut self, extra: BoltDict) -> Result<(), BoltError> {
        self.send(&ClientMessage::Begin { extra }).await?;
        match self.recv().await? {
            ServerMessage::Success { .. } => Ok(()),
            ServerMessage::Failure { metadata } => Err(BoltError::Transaction(
                metadata
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("BEGIN failed")
                    .to_string(),
            )),
            other => Err(BoltError::Protocol(format!(
                "expected SUCCESS after BEGIN, got {other:?}"
            ))),
        }
    }

    /// Sends COMMIT and expects SUCCESS.
    pub async fn commit(&mut self) -> Result<BoltDict, BoltError> {
        self.send(&ClientMessage::Commit).await?;
        match self.recv().await? {
            ServerMessage::Success { metadata } => Ok(metadata),
            ServerMessage::Failure { metadata } => Err(BoltError::Transaction(
                metadata
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("COMMIT failed")
                    .to_string(),
            )),
            other => Err(BoltError::Protocol(format!(
                "expected SUCCESS after COMMIT, got {other:?}"
            ))),
        }
    }

    /// Sends ROLLBACK and expects SUCCESS.
    pub async fn rollback(&mut self) -> Result<(), BoltError> {
        self.send(&ClientMessage::Rollback).await?;
        match self.recv().await? {
            ServerMessage::Success { .. } => Ok(()),
            ServerMessage::Failure { metadata } => Err(BoltError::Transaction(
                metadata
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ROLLBACK failed")
                    .to_string(),
            )),
            other => Err(BoltError::Protocol(format!(
                "expected SUCCESS after ROLLBACK, got {other:?}"
            ))),
        }
    }

    /// Sends RESET and expects SUCCESS.
    pub async fn reset(&mut self) -> Result<(), BoltError> {
        self.send(&ClientMessage::Reset).await?;
        match self.recv().await? {
            ServerMessage::Success { .. } => Ok(()),
            ServerMessage::Failure { metadata } => Err(BoltError::Protocol(
                metadata
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("RESET failed")
                    .to_string(),
            )),
            other => Err(BoltError::Protocol(format!(
                "expected SUCCESS after RESET, got {other:?}"
            ))),
        }
    }
}

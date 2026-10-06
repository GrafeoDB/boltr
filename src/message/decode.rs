//! Decode Bolt messages from PackStream bytes.

use bytes::Buf;

use super::{ClientMessage, ServerMessage, sig};
use crate::error::BoltError;
use crate::packstream::decode::decode_value;
use crate::packstream::marker::TINY_STRUCT_NIBBLE;
use crate::types::{BoltDict, BoltValue};

/// Decodes a client message from PackStream bytes.
///
/// Returns a protocol error (never panics) when the message is truncated, is
/// not a PackStream structure, has an unknown signature, has too few fields
/// or contains malformed values.
pub fn decode_client_message(data: &[u8]) -> Result<ClientMessage, BoltError> {
    let mut buf = data;
    let (field_count, tag) = read_message_header(&mut buf)?;

    match tag {
        sig::HELLO => {
            expect_fields("HELLO", field_count, 1)?;
            let extra = require_dict(decode_value(&mut buf)?)?;
            Ok(ClientMessage::Hello { extra })
        }
        sig::LOGON => {
            expect_fields("LOGON", field_count, 1)?;
            let auth = require_dict(decode_value(&mut buf)?)?;
            Ok(ClientMessage::Logon { auth })
        }
        sig::LOGOFF => Ok(ClientMessage::Logoff),
        sig::GOODBYE => Ok(ClientMessage::Goodbye),
        sig::RESET => Ok(ClientMessage::Reset),
        sig::RUN => {
            expect_fields("RUN", field_count, 3)?;
            let query = require_string(decode_value(&mut buf)?)?;
            let parameters = require_dict(decode_value(&mut buf)?)?;
            let extra = require_dict(decode_value(&mut buf)?)?;
            Ok(ClientMessage::Run {
                query,
                parameters,
                extra,
            })
        }
        sig::PULL => {
            expect_fields("PULL", field_count, 1)?;
            let extra = require_dict(decode_value(&mut buf)?)?;
            Ok(ClientMessage::Pull { extra })
        }
        sig::DISCARD => {
            expect_fields("DISCARD", field_count, 1)?;
            let extra = require_dict(decode_value(&mut buf)?)?;
            Ok(ClientMessage::Discard { extra })
        }
        sig::BEGIN => {
            expect_fields("BEGIN", field_count, 1)?;
            let extra = require_dict(decode_value(&mut buf)?)?;
            Ok(ClientMessage::Begin { extra })
        }
        sig::COMMIT => Ok(ClientMessage::Commit),
        sig::ROLLBACK => Ok(ClientMessage::Rollback),
        sig::ROUTE => {
            expect_fields("ROUTE", field_count, 3)?;
            let routing = require_dict(decode_value(&mut buf)?)?;
            let bookmarks_val = require_list(decode_value(&mut buf)?)?;
            let bookmarks = bookmarks_val
                .into_iter()
                .filter_map(|v| match v {
                    BoltValue::String(s) => Some(s),
                    _ => None,
                })
                .collect();
            let extra = require_dict(decode_value(&mut buf)?)?;
            Ok(ClientMessage::Route {
                routing,
                bookmarks,
                extra,
            })
        }
        sig::TELEMETRY => {
            expect_fields("TELEMETRY", field_count, 1)?;
            let api = match decode_value(&mut buf)? {
                BoltValue::Integer(i) => i,
                _ => 0,
            };
            Ok(ClientMessage::Telemetry { api })
        }
        _ => Err(BoltError::Protocol(format!(
            "unknown client message tag: 0x{tag:02X}"
        ))),
    }
}

/// Decodes a server message from PackStream bytes.
///
/// Returns a protocol error (never panics) on malformed input.
pub fn decode_server_message(data: &[u8]) -> Result<ServerMessage, BoltError> {
    let mut buf = data;
    let (field_count, tag) = read_message_header(&mut buf)?;

    match tag {
        sig::SUCCESS => {
            expect_fields("SUCCESS", field_count, 1)?;
            let metadata = require_dict(decode_value(&mut buf)?)?;
            Ok(ServerMessage::Success { metadata })
        }
        sig::RECORD => {
            expect_fields("RECORD", field_count, 1)?;
            let data = require_list(decode_value(&mut buf)?)?;
            Ok(ServerMessage::Record { data })
        }
        sig::FAILURE => {
            expect_fields("FAILURE", field_count, 1)?;
            let metadata = require_dict(decode_value(&mut buf)?)?;
            Ok(ServerMessage::Failure { metadata })
        }
        sig::IGNORED => Ok(ServerMessage::Ignored),
        _ => Err(BoltError::Protocol(format!(
            "unknown server message tag: 0x{tag:02X}"
        ))),
    }
}

fn read_u8(buf: &mut &[u8]) -> Result<u8, BoltError> {
    if buf.has_remaining() {
        Ok(buf.get_u8())
    } else {
        Err(BoltError::Protocol("unexpected end of message".into()))
    }
}

/// Reads the structure marker and signature byte that start every message.
fn read_message_header(buf: &mut &[u8]) -> Result<(u8, u8), BoltError> {
    let marker = read_u8(buf)?;
    if marker & 0xF0 != TINY_STRUCT_NIBBLE {
        return Err(BoltError::Protocol(format!(
            "message must be a PackStream structure, got marker 0x{marker:02X}"
        )));
    }
    let tag = read_u8(buf)?;
    Ok((marker & 0x0F, tag))
}

fn expect_fields(msg_name: &str, got: u8, expected: u8) -> Result<(), BoltError> {
    if got < expected {
        Err(BoltError::Protocol(format!(
            "{msg_name} expects at least {expected} fields, got {got}"
        )))
    } else {
        Ok(())
    }
}

fn require_string(v: BoltValue) -> Result<String, BoltError> {
    match v {
        BoltValue::String(s) => Ok(s),
        other => Err(BoltError::Protocol(format!(
            "expected String, got {}",
            other.type_name()
        ))),
    }
}

fn require_dict(v: BoltValue) -> Result<BoltDict, BoltError> {
    match v {
        BoltValue::Dict(d) => Ok(d),
        other => Err(BoltError::Protocol(format!(
            "expected Dictionary, got {}",
            other.type_name()
        ))),
    }
}

fn require_list(v: BoltValue) -> Result<Vec<BoltValue>, BoltError> {
    match v {
        BoltValue::List(l) => Ok(l),
        other => Err(BoltError::Protocol(format!(
            "expected List, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::encode::{encode_client_message, encode_server_message};
    use bytes::BytesMut;

    fn round_trip_client(msg: &ClientMessage) -> ClientMessage {
        let mut buf = BytesMut::new();
        encode_client_message(&mut buf, msg);
        decode_client_message(&buf).expect("decode failed")
    }

    fn round_trip_server(msg: &ServerMessage) -> ServerMessage {
        let mut buf = BytesMut::new();
        encode_server_message(&mut buf, msg);
        decode_server_message(&buf).expect("decode failed")
    }

    #[test]
    fn round_trip_hello() {
        let msg = ClientMessage::Hello {
            extra: BoltDict::from([(
                "user_agent".to_string(),
                BoltValue::String("test/1.0".into()),
            )]),
        };
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_logon() {
        let msg = ClientMessage::Logon {
            auth: BoltDict::from([
                ("scheme".to_string(), BoltValue::String("basic".into())),
                ("principal".to_string(), BoltValue::String("neo4j".into())),
                (
                    "credentials".to_string(),
                    BoltValue::String("password".into()),
                ),
            ]),
        };
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_run() {
        let msg = ClientMessage::Run {
            query: "RETURN 1".into(),
            parameters: BoltDict::new(),
            extra: BoltDict::from([("db".to_string(), BoltValue::String("neo4j".into()))]),
        };
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_zero_field_messages() {
        for msg in [
            ClientMessage::Logoff,
            ClientMessage::Goodbye,
            ClientMessage::Reset,
            ClientMessage::Commit,
            ClientMessage::Rollback,
        ] {
            assert_eq!(round_trip_client(&msg), msg);
        }
    }

    #[test]
    fn round_trip_pull() {
        let msg = ClientMessage::pull_all();
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_success() {
        let msg = ServerMessage::Success {
            metadata: BoltDict::from([(
                "server".to_string(),
                BoltValue::String("GrafeoDB/0.4.4".into()),
            )]),
        };
        assert_eq!(round_trip_server(&msg), msg);
    }

    #[test]
    fn round_trip_record() {
        let msg = ServerMessage::Record {
            data: vec![BoltValue::Integer(1), BoltValue::String("hello".into())],
        };
        assert_eq!(round_trip_server(&msg), msg);
    }

    #[test]
    fn round_trip_failure() {
        let msg = ServerMessage::Failure {
            metadata: BoltDict::from([
                (
                    "code".to_string(),
                    BoltValue::String("Neo.ClientError.Statement.SyntaxError".into()),
                ),
                ("message".to_string(), BoltValue::String("bad query".into())),
            ]),
        };
        assert_eq!(round_trip_server(&msg), msg);
    }

    #[test]
    fn round_trip_ignored() {
        assert_eq!(
            round_trip_server(&ServerMessage::Ignored),
            ServerMessage::Ignored
        );
    }

    #[test]
    fn round_trip_route() {
        let msg = ClientMessage::Route {
            routing: BoltDict::from([(
                "address".to_string(),
                BoltValue::String("localhost:7687".into()),
            )]),
            bookmarks: vec!["bk:1".into(), "bk:2".into()],
            extra: BoltDict::from([("db".to_string(), BoltValue::String("neo4j".into()))]),
        };
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_telemetry() {
        let msg = ClientMessage::Telemetry { api: 42 };
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_every_client_message() {
        let extra = BoltDict::from([("n".to_string(), BoltValue::Integer(7))]);
        for msg in [
            ClientMessage::Hello {
                extra: extra.clone(),
            },
            ClientMessage::Logon {
                auth: extra.clone(),
            },
            ClientMessage::Logoff,
            ClientMessage::Goodbye,
            ClientMessage::Reset,
            ClientMessage::Run {
                query: "RETURN $x".into(),
                parameters: BoltDict::from([("x".to_string(), BoltValue::Float(1.5))]),
                extra: extra.clone(),
            },
            ClientMessage::Pull {
                extra: extra.clone(),
            },
            ClientMessage::Discard {
                extra: extra.clone(),
            },
            ClientMessage::Begin {
                extra: extra.clone(),
            },
            ClientMessage::Commit,
            ClientMessage::Rollback,
            ClientMessage::Route {
                routing: BoltDict::new(),
                bookmarks: vec![],
                extra,
            },
            ClientMessage::Telemetry { api: -1 },
        ] {
            assert_eq!(round_trip_client(&msg), msg);
        }
    }

    #[test]
    fn empty_and_truncated_messages_are_rejected() {
        assert!(decode_client_message(&[]).is_err());
        assert!(decode_client_message(&[0xB1]).is_err());
        assert!(decode_server_message(&[]).is_err());
        assert!(decode_server_message(&[0xB1]).is_err());
        // RUN missing its parameters and extra dictionaries.
        assert!(decode_client_message(&[0xB3, sig::RUN, 0x81, b'x']).is_err());
        // SUCCESS whose metadata dictionary is cut short.
        assert!(decode_server_message(&[0xB1, sig::SUCCESS, 0xA1, 0x81]).is_err());
    }

    /// Regression: the first byte was only masked for its field count, so a
    /// message starting with any byte (for example a tiny int) was accepted
    /// as a structure.
    #[test]
    fn non_structure_marker_is_rejected() {
        for first in [0x01u8, 0x81, 0x91, 0xA1, 0xC0, 0xF1] {
            let err = decode_client_message(&[first, sig::RESET]).unwrap_err();
            assert!(
                err.to_string().contains("structure"),
                "0x{first:02X}: {err}"
            );
            assert!(decode_server_message(&[first, sig::IGNORED]).is_err());
        }
    }

    #[test]
    fn unknown_signatures_are_rejected() {
        let err = decode_client_message(&[0xB0, 0x55]).unwrap_err();
        assert!(err.to_string().contains("0x55"), "{err}");
        // A server message signature is not a client message and vice versa.
        assert!(decode_client_message(&[0xB0, sig::IGNORED]).is_err());
        assert!(decode_server_message(&[0xB0, sig::RESET]).is_err());
    }

    #[test]
    fn too_few_fields_are_rejected() {
        for (tag, name) in [
            (sig::HELLO, "HELLO"),
            (sig::LOGON, "LOGON"),
            (sig::PULL, "PULL"),
            (sig::DISCARD, "DISCARD"),
            (sig::BEGIN, "BEGIN"),
            (sig::TELEMETRY, "TELEMETRY"),
        ] {
            let err = decode_client_message(&[0xB0, tag]).unwrap_err();
            assert!(err.to_string().contains(name), "{name}: {err}");
        }
        assert!(decode_client_message(&[0xB2, sig::RUN, 0x80, 0xA0]).is_err());
        assert!(decode_client_message(&[0xB2, sig::ROUTE, 0xA0, 0x90]).is_err());
        assert!(decode_server_message(&[0xB0, sig::SUCCESS]).is_err());
        assert!(decode_server_message(&[0xB0, sig::RECORD]).is_err());
        assert!(decode_server_message(&[0xB0, sig::FAILURE]).is_err());
    }

    #[test]
    fn wrong_field_types_are_rejected() {
        // RUN whose query is an integer.
        assert!(decode_client_message(&[0xB3, sig::RUN, 0x01, 0xA0, 0xA0]).is_err());
        // HELLO whose extra is a list.
        assert!(decode_client_message(&[0xB1, sig::HELLO, 0x90]).is_err());
        // ROUTE whose bookmarks are a dictionary.
        assert!(decode_client_message(&[0xB3, sig::ROUTE, 0xA0, 0xA0, 0xA0]).is_err());
        // RECORD whose data is a dictionary.
        assert!(decode_server_message(&[0xB1, sig::RECORD, 0xA0]).is_err());
    }
}

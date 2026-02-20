//! Decode Bolt messages from PackStream bytes.

use bytes::Buf;

use super::{sig, ClientMessage, ServerMessage};
use crate::error::BoltError;
use crate::packstream::decode::decode_value;
use crate::types::{BoltDict, BoltValue};

/// Decodes a client message from PackStream bytes.
pub fn decode_client_message(data: &[u8]) -> Result<ClientMessage, BoltError> {
    let mut buf = data;
    let marker = read_u8(&mut buf)?;
    let field_count = marker & 0x0F;
    let tag = read_u8(&mut buf)?;

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
        _ => Err(BoltError::Protocol(format!(
            "unknown client message tag: 0x{tag:02X}"
        ))),
    }
}

/// Decodes a server message from PackStream bytes.
pub fn decode_server_message(data: &[u8]) -> Result<ServerMessage, BoltError> {
    let mut buf = data;
    let marker = read_u8(&mut buf)?;
    let field_count = marker & 0x0F;
    let tag = read_u8(&mut buf)?;

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
            "expected string, got: {other}"
        ))),
    }
}

fn require_dict(v: BoltValue) -> Result<BoltDict, BoltError> {
    match v {
        BoltValue::Dict(d) => Ok(d),
        other => Err(BoltError::Protocol(format!("expected dict, got: {other}"))),
    }
}

fn require_list(v: BoltValue) -> Result<Vec<BoltValue>, BoltError> {
    match v {
        BoltValue::List(l) => Ok(l),
        other => Err(BoltError::Protocol(format!("expected list, got: {other}"))),
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
            extra: BoltDict::from([
                ("user_agent".to_string(), BoltValue::String("test/1.0".into())),
            ]),
        };
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_logon() {
        let msg = ClientMessage::Logon {
            auth: BoltDict::from([
                ("scheme".to_string(), BoltValue::String("basic".into())),
                ("principal".to_string(), BoltValue::String("neo4j".into())),
                ("credentials".to_string(), BoltValue::String("password".into())),
            ]),
        };
        assert_eq!(round_trip_client(&msg), msg);
    }

    #[test]
    fn round_trip_run() {
        let msg = ClientMessage::Run {
            query: "RETURN 1".into(),
            parameters: BoltDict::new(),
            extra: BoltDict::from([
                ("db".to_string(), BoltValue::String("neo4j".into())),
            ]),
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
            metadata: BoltDict::from([
                ("server".to_string(), BoltValue::String("GrafeoDB/0.4.4".into())),
            ]),
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
                ("code".to_string(), BoltValue::String("Neo.ClientError.Statement.SyntaxError".into())),
                ("message".to_string(), BoltValue::String("bad query".into())),
            ]),
        };
        assert_eq!(round_trip_server(&msg), msg);
    }

    #[test]
    fn round_trip_ignored() {
        assert_eq!(round_trip_server(&ServerMessage::Ignored), ServerMessage::Ignored);
    }
}

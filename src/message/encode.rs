//! Encode Bolt messages to PackStream bytes.

use bytes::BytesMut;

use super::{sig, ClientMessage, ServerMessage};
use crate::packstream::encode as ps;
use crate::types::BoltValue;

/// Encodes a client message into PackStream bytes.
pub fn encode_client_message(buf: &mut BytesMut, msg: &ClientMessage) {
    match msg {
        ClientMessage::Hello { extra } => {
            ps::encode_struct_header(buf, sig::HELLO, 1);
            ps::encode_dict(buf, extra);
        }
        ClientMessage::Logon { auth } => {
            ps::encode_struct_header(buf, sig::LOGON, 1);
            ps::encode_dict(buf, auth);
        }
        ClientMessage::Logoff => {
            ps::encode_struct_header(buf, sig::LOGOFF, 0);
        }
        ClientMessage::Goodbye => {
            ps::encode_struct_header(buf, sig::GOODBYE, 0);
        }
        ClientMessage::Reset => {
            ps::encode_struct_header(buf, sig::RESET, 0);
        }
        ClientMessage::Run {
            query,
            parameters,
            extra,
        } => {
            ps::encode_struct_header(buf, sig::RUN, 3);
            ps::encode_string(buf, query);
            ps::encode_dict(buf, parameters);
            ps::encode_dict(buf, extra);
        }
        ClientMessage::Pull { extra } => {
            ps::encode_struct_header(buf, sig::PULL, 1);
            ps::encode_dict(buf, extra);
        }
        ClientMessage::Discard { extra } => {
            ps::encode_struct_header(buf, sig::DISCARD, 1);
            ps::encode_dict(buf, extra);
        }
        ClientMessage::Begin { extra } => {
            ps::encode_struct_header(buf, sig::BEGIN, 1);
            ps::encode_dict(buf, extra);
        }
        ClientMessage::Commit => {
            ps::encode_struct_header(buf, sig::COMMIT, 0);
        }
        ClientMessage::Rollback => {
            ps::encode_struct_header(buf, sig::ROLLBACK, 0);
        }
    }
}

/// Encodes a server message into PackStream bytes.
pub fn encode_server_message(buf: &mut BytesMut, msg: &ServerMessage) {
    match msg {
        ServerMessage::Success { metadata } => {
            ps::encode_struct_header(buf, sig::SUCCESS, 1);
            ps::encode_dict(buf, metadata);
        }
        ServerMessage::Record { data } => {
            ps::encode_struct_header(buf, sig::RECORD, 1);
            ps::encode_list(buf, data);
        }
        ServerMessage::Failure { metadata } => {
            ps::encode_struct_header(buf, sig::FAILURE, 1);
            ps::encode_dict(buf, metadata);
        }
        ServerMessage::Ignored => {
            ps::encode_struct_header(buf, sig::IGNORED, 0);
        }
    }
}

/// Convenience: encode a server SUCCESS with the given key-value metadata.
pub fn encode_success(buf: &mut BytesMut, metadata: &std::collections::HashMap<String, BoltValue>) {
    encode_server_message(buf, &ServerMessage::Success { metadata: metadata.clone() });
}

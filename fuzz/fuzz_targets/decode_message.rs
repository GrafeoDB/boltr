//! Bolt message decoding of arbitrary bytes (both directions): must never
//! panic, and accepted messages must round-trip.

#![no_main]

use boltr::message::decode::{decode_client_message, decode_server_message};
use boltr::message::encode::{encode_client_message, encode_server_message};
use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(msg) = decode_client_message(data) {
        let mut buf = BytesMut::new();
        encode_client_message(&mut buf, &msg);
        decode_client_message(&buf).expect("re-encoded client message must decode");
    }
    if let Ok(msg) = decode_server_message(data) {
        let mut buf = BytesMut::new();
        encode_server_message(&mut buf, &msg);
        decode_server_message(&buf).expect("re-encoded server message must decode");
    }
});

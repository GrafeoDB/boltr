//! PackStream decoding of arbitrary bytes: must never panic, overflow the
//! stack or allocate unboundedly, and every value it accepts must survive an
//! encode/decode round trip.

#![no_main]

use boltr::packstream::{decode_value, encode_value};
use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut cursor = data;
    let Ok(value) = decode_value(&mut cursor) else {
        return;
    };
    let mut encoded = BytesMut::new();
    encode_value(&mut encoded, &value);
    let mut again = &encoded[..];
    let decoded = decode_value(&mut again).expect("re-encoded value must decode");
    assert!(
        again.is_empty(),
        "re-encoded value must be consumed exactly"
    );
    // NaN is the only value not equal to itself.
    if !format!("{value:?}").contains("NaN") {
        assert_eq!(decoded, value);
    }
});

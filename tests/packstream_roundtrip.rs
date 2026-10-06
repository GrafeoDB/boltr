//! PackStream, message and chunk-framing round-trip tests: property tests
//! over arbitrary values and messages, exact wire bytes at every size and
//! integer boundary, and "never panics" checks on arbitrary and mutated input.

use bytes::BytesMut;
use proptest::collection::{hash_map, vec};
use proptest::prelude::*;

use boltr::chunk::{ChunkReader, ChunkWriter};
use boltr::message::decode::{decode_client_message, decode_server_message};
use boltr::message::encode::{encode_client_message, encode_server_message};
use boltr::message::{ClientMessage, ServerMessage};
use boltr::packstream::marker;
use boltr::packstream::{decode_value, encode_value};
use boltr::types::{
    BoltDate, BoltDateTime, BoltDateTimeZoneId, BoltDict, BoltDuration, BoltLocalDateTime,
    BoltLocalTime, BoltNode, BoltPath, BoltPoint2D, BoltPoint3D, BoltRelationship, BoltTime,
    BoltUnboundRelationship, BoltValue,
};

fn encode(value: &BoltValue) -> BytesMut {
    let mut buf = BytesMut::new();
    encode_value(&mut buf, value);
    buf
}

/// Decodes one value and asserts that it consumed the whole input.
fn decode_all(bytes: &[u8]) -> Result<BoltValue, boltr::BoltError> {
    let mut cursor = bytes;
    let value = decode_value(&mut cursor)?;
    assert!(cursor.is_empty(), "{} trailing bytes", cursor.len());
    Ok(value)
}

fn round_trip(value: &BoltValue) -> BoltValue {
    decode_all(&encode(value)).expect("decode")
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Integers concentrated around every encoding boundary.
fn boundary_int() -> impl Strategy<Value = i64> {
    let edges = [
        -16i64,
        127,
        i64::from(i8::MIN),
        i64::from(i8::MAX),
        i64::from(i16::MIN),
        i64::from(i16::MAX),
        i64::from(i32::MIN),
        i64::from(i32::MAX),
        i64::MIN,
        i64::MAX,
        0,
    ];
    (prop::sample::select(edges.to_vec()), -2i64..=2)
        .prop_map(|(edge, delta)| edge.saturating_add(delta))
}

fn finite_float() -> impl Strategy<Value = f64> {
    any::<f64>().prop_filter("NaN never equals itself", |f| !f.is_nan())
}

fn dict_of(inner: impl Strategy<Value = BoltValue>) -> impl Strategy<Value = BoltDict> {
    hash_map(".{0,12}", inner, 0..6)
}

fn node(inner: impl Strategy<Value = BoltValue>) -> impl Strategy<Value = BoltNode> {
    (any::<i64>(), vec(".{0,8}", 0..4), dict_of(inner), ".{0,16}").prop_map(
        |(id, labels, properties, element_id)| BoltNode {
            id,
            labels,
            properties,
            element_id,
        },
    )
}

fn unbound(
    inner: impl Strategy<Value = BoltValue>,
) -> impl Strategy<Value = BoltUnboundRelationship> {
    (any::<i64>(), ".{0,8}", dict_of(inner), ".{0,16}").prop_map(
        |(id, rel_type, properties, element_id)| BoltUnboundRelationship {
            id,
            rel_type,
            properties,
            element_id,
        },
    )
}

fn leaf() -> impl Strategy<Value = BoltValue> {
    prop_oneof![
        Just(BoltValue::Null),
        any::<bool>().prop_map(BoltValue::Boolean),
        any::<i64>().prop_map(BoltValue::Integer),
        boundary_int().prop_map(BoltValue::Integer),
        finite_float().prop_map(BoltValue::Float),
        ".{0,40}".prop_map(BoltValue::String),
        vec(any::<u8>(), 0..64).prop_map(BoltValue::Bytes),
        any::<i64>().prop_map(|days| BoltValue::Date(BoltDate { days })),
        (any::<i64>(), any::<i64>()).prop_map(|(nanoseconds, tz_offset_seconds)| {
            BoltValue::Time(BoltTime {
                nanoseconds,
                tz_offset_seconds,
            })
        }),
        any::<i64>().prop_map(|nanoseconds| BoltValue::LocalTime(BoltLocalTime { nanoseconds })),
        (any::<i64>(), any::<i64>(), any::<i64>()).prop_map(
            |(seconds, nanoseconds, tz_offset_seconds)| BoltValue::DateTime(BoltDateTime {
                seconds,
                nanoseconds,
                tz_offset_seconds,
            })
        ),
        (any::<i64>(), any::<i64>(), ".{0,24}").prop_map(|(seconds, nanoseconds, tz_id)| {
            BoltValue::DateTimeZoneId(BoltDateTimeZoneId {
                seconds,
                nanoseconds,
                tz_id,
            })
        }),
        (any::<i64>(), any::<i64>()).prop_map(|(seconds, nanoseconds)| {
            BoltValue::LocalDateTime(BoltLocalDateTime {
                seconds,
                nanoseconds,
            })
        }),
        (any::<i64>(), any::<i64>(), any::<i64>(), any::<i64>()).prop_map(
            |(months, days, seconds, nanoseconds)| BoltValue::Duration(BoltDuration {
                months,
                days,
                seconds,
                nanoseconds,
            })
        ),
        (any::<i64>(), finite_float(), finite_float())
            .prop_map(|(srid, x, y)| BoltValue::Point2D(BoltPoint2D { srid, x, y })),
        (any::<i64>(), finite_float(), finite_float(), finite_float())
            .prop_map(|(srid, x, y, z)| BoltValue::Point3D(BoltPoint3D { srid, x, y, z })),
    ]
}

/// Any `BoltValue`, including nested containers and graph structures.
fn value() -> impl Strategy<Value = BoltValue> {
    leaf().prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..6).prop_map(BoltValue::List),
            dict_of(inner.clone()).prop_map(BoltValue::Dict),
            node(inner.clone()).prop_map(BoltValue::Node),
            unbound(inner.clone()).prop_map(BoltValue::UnboundRelationship),
            (
                any::<i64>(),
                any::<i64>(),
                any::<i64>(),
                ".{0,8}",
                dict_of(inner.clone()),
                ".{0,8}",
                ".{0,8}",
                ".{0,8}"
            )
                .prop_map(
                    |(id, start, end, rel_type, properties, element_id, start_id, end_id)| {
                        BoltValue::Relationship(BoltRelationship {
                            id,
                            start_node_id: start,
                            end_node_id: end,
                            rel_type,
                            properties,
                            element_id,
                            start_element_id: start_id,
                            end_element_id: end_id,
                        })
                    }
                ),
            (
                vec(node(inner.clone()), 0..3),
                vec(unbound(inner), 0..3),
                vec(any::<i64>(), 0..6)
            )
                .prop_map(|(nodes, rels, indices)| BoltValue::Path(BoltPath {
                    nodes,
                    rels,
                    indices,
                })),
        ]
    })
}

fn client_message() -> impl Strategy<Value = ClientMessage> {
    let dict = || dict_of(value());
    prop_oneof![
        dict().prop_map(|extra| ClientMessage::Hello { extra }),
        dict().prop_map(|auth| ClientMessage::Logon { auth }),
        Just(ClientMessage::Logoff),
        Just(ClientMessage::Goodbye),
        Just(ClientMessage::Reset),
        (".{0,64}", dict(), dict()).prop_map(|(query, parameters, extra)| ClientMessage::Run {
            query,
            parameters,
            extra
        }),
        dict().prop_map(|extra| ClientMessage::Pull { extra }),
        dict().prop_map(|extra| ClientMessage::Discard { extra }),
        dict().prop_map(|extra| ClientMessage::Begin { extra }),
        Just(ClientMessage::Commit),
        Just(ClientMessage::Rollback),
        (dict(), vec(".{0,16}", 0..4), dict()).prop_map(|(routing, bookmarks, extra)| {
            ClientMessage::Route {
                routing,
                bookmarks,
                extra,
            }
        }),
        any::<i64>().prop_map(|api| ClientMessage::Telemetry { api }),
    ]
}

fn server_message() -> impl Strategy<Value = ServerMessage> {
    prop_oneof![
        dict_of(value()).prop_map(|metadata| ServerMessage::Success { metadata }),
        vec(value(), 0..6).prop_map(|data| ServerMessage::Record { data }),
        dict_of(value()).prop_map(|metadata| ServerMessage::Failure { metadata }),
        Just(ServerMessage::Ignored),
    ]
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(future)
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn any_value_round_trips(v in value()) {
        prop_assert_eq!(round_trip(&v), v);
    }

    #[test]
    fn concatenated_values_decode_in_sequence(values in vec(value(), 0..8)) {
        let mut buf = BytesMut::new();
        for v in &values {
            encode_value(&mut buf, v);
        }
        let mut cursor = &buf[..];
        for v in &values {
            prop_assert_eq!(&decode_value(&mut cursor).unwrap(), v);
        }
        prop_assert!(cursor.is_empty());
    }

    /// No strict prefix of a value's encoding decodes: truncation is always
    /// detected, never silently accepted.
    #[test]
    fn truncated_encodings_are_errors(v in value(), cut in any::<prop::sample::Index>()) {
        let bytes = encode(&v);
        let len = cut.index(bytes.len());
        let mut cursor = &bytes[..len];
        prop_assert!(decode_value(&mut cursor).is_err());
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in vec(any::<u8>(), 0..512)) {
        let mut cursor = &bytes[..];
        let _ = decode_value(&mut cursor);
        let _ = decode_client_message(&bytes);
        let _ = decode_server_message(&bytes);
    }

    #[test]
    fn mutated_encodings_never_panic(
        v in value(),
        flips in vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
    ) {
        let mut bytes = encode(&v).to_vec();
        for (index, byte) in flips {
            let i = index.index(bytes.len());
            bytes[i] = byte;
        }
        let mut cursor = &bytes[..];
        let _ = decode_value(&mut cursor);
    }

    #[test]
    fn client_messages_round_trip(msg in client_message()) {
        let mut buf = BytesMut::new();
        encode_client_message(&mut buf, &msg);
        prop_assert_eq!(decode_client_message(&buf).unwrap(), msg);
    }

    #[test]
    fn server_messages_round_trip(msg in server_message()) {
        let mut buf = BytesMut::new();
        encode_server_message(&mut buf, &msg);
        prop_assert_eq!(decode_server_message(&buf).unwrap(), msg);
    }

    /// Writer output, re-split at arbitrary chunk boundaries (including
    /// boundaries inside markers, sizes and UTF-8 sequences), still
    /// reassembles to the same message.
    #[test]
    fn messages_survive_any_chunking(
        msg in client_message(),
        sizes in vec(1usize..=40, 1..64),
    ) {
        let mut payload = BytesMut::new();
        encode_client_message(&mut payload, &msg);

        let mut framed = Vec::new();
        let mut rest = &payload[..];
        let mut size_iter = sizes.iter().cycle();
        while !rest.is_empty() {
            let take = (*size_iter.next().unwrap()).min(rest.len());
            framed.extend_from_slice(&u16::try_from(take).unwrap().to_be_bytes());
            framed.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
        }
        framed.extend_from_slice(&[0, 0]);

        let reassembled = block_on(async {
            ChunkReader::new(std::io::Cursor::new(framed)).read_message().await
        })
        .unwrap();
        prop_assert_eq!(&reassembled[..], &payload[..]);
        prop_assert_eq!(decode_client_message(&reassembled).unwrap(), msg);
    }
}

proptest! {
    // Large payloads: fewer cases keep the suite fast in debug builds.
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn chunk_writer_and_reader_round_trip(payloads in vec(vec(any::<u8>(), 0..70_000), 1..4)) {
        let output = block_on(async {
            let mut output = Vec::new();
            let mut writer = ChunkWriter::new(&mut output);
            for payload in &payloads {
                writer.write_message(payload).await.unwrap();
            }
            output
        });
        let decoded = block_on(async {
            let mut reader = ChunkReader::new(std::io::Cursor::new(output));
            let mut decoded = Vec::new();
            for _ in &payloads {
                decoded.push(reader.read_message().await.unwrap().to_vec());
            }
            decoded
        });
        prop_assert_eq!(decoded, payloads);
    }
}

// ---------------------------------------------------------------------------
// Exact wire format at every boundary
// ---------------------------------------------------------------------------

#[test]
fn integer_encoding_boundaries() {
    // (value, marker or None for TINY_INT, encoded length)
    let cases: &[(i64, Option<u8>, usize)] = &[
        (-16, None, 1),
        (-1, None, 1),
        (0, None, 1),
        (127, None, 1),
        (-17, Some(marker::INT_8), 2),
        (-128, Some(marker::INT_8), 2),
        (128, Some(marker::INT_16), 3),
        (-129, Some(marker::INT_16), 3),
        (32_767, Some(marker::INT_16), 3),
        (-32_768, Some(marker::INT_16), 3),
        (32_768, Some(marker::INT_32), 5),
        (-32_769, Some(marker::INT_32), 5),
        (i64::from(i32::MAX), Some(marker::INT_32), 5),
        (i64::from(i32::MIN), Some(marker::INT_32), 5),
        (i64::from(i32::MAX) + 1, Some(marker::INT_64), 9),
        (i64::from(i32::MIN) - 1, Some(marker::INT_64), 9),
        (i64::MAX, Some(marker::INT_64), 9),
        (i64::MIN, Some(marker::INT_64), 9),
    ];
    for &(value, expected_marker, expected_len) in cases {
        let bytes = encode(&BoltValue::Integer(value));
        assert_eq!(bytes.len(), expected_len, "{value}");
        match expected_marker {
            Some(m) => assert_eq!(bytes[0], m, "{value}"),
            None => assert_eq!(bytes[0], value as u8, "{value}"),
        }
        assert_eq!(
            round_trip(&BoltValue::Integer(value)),
            BoltValue::Integer(value)
        );
    }
}

#[test]
fn every_tiny_int_marker_decodes() {
    for byte in 0x00..=0x7Fu8 {
        assert_eq!(
            decode_all(&[byte]).unwrap(),
            BoltValue::Integer(i64::from(byte))
        );
    }
    for byte in 0xF0..=0xFFu8 {
        assert_eq!(
            decode_all(&[byte]).unwrap(),
            BoltValue::Integer(i64::from(byte as i8))
        );
    }
}

/// Expected header for a container of `len` items/bytes.
fn header(tiny_nibble: Option<u8>, family: u8, len: usize) -> Vec<u8> {
    match (tiny_nibble, len) {
        (Some(nibble), 0..=15) => vec![nibble | len as u8],
        (_, 0..=255) => vec![family, len as u8],
        (_, 256..=65_535) => {
            let mut h = vec![family + 1];
            h.extend_from_slice(&(len as u16).to_be_bytes());
            h
        }
        _ => {
            let mut h = vec![family + 2];
            h.extend_from_slice(&(len as u32).to_be_bytes());
            h
        }
    }
}

const SIZE_BOUNDARIES: [usize; 9] = [0, 1, 15, 16, 255, 256, 65_535, 65_536, 70_000];

#[test]
fn string_size_markers() {
    for len in SIZE_BOUNDARIES {
        let value = BoltValue::String("a".repeat(len));
        let bytes = encode(&value);
        let expected = header(Some(marker::TINY_STRING_NIBBLE), marker::STRING_8, len);
        assert_eq!(&bytes[..expected.len()], &expected[..], "len {len}");
        assert_eq!(bytes.len(), expected.len() + len);
        assert_eq!(decode_all(&bytes).unwrap(), value);
    }
    // Sizes count bytes, not characters: 8 two-byte characters need STRING_8.
    let bytes = encode(&BoltValue::String("é".repeat(8)));
    assert_eq!(&bytes[..2], &[marker::STRING_8, 16]);
}

#[test]
fn bytes_size_markers() {
    for len in SIZE_BOUNDARIES {
        let value = BoltValue::Bytes(vec![0xAB; len]);
        let bytes = encode(&value);
        // Bytes have no tiny form.
        let expected = header(None, marker::BYTES_8, len);
        assert_eq!(&bytes[..expected.len()], &expected[..], "len {len}");
        assert_eq!(decode_all(&bytes).unwrap(), value);
    }
}

#[test]
fn list_size_markers() {
    for len in SIZE_BOUNDARIES {
        let value = BoltValue::List(vec![BoltValue::Null; len]);
        let bytes = encode(&value);
        let expected = header(Some(marker::TINY_LIST_NIBBLE), marker::LIST_8, len);
        assert_eq!(&bytes[..expected.len()], &expected[..], "len {len}");
        assert_eq!(decode_all(&bytes).unwrap(), value);
    }
}

#[test]
fn dict_size_markers() {
    for len in SIZE_BOUNDARIES {
        let dict: BoltDict = (0..len)
            .map(|i| (i.to_string(), BoltValue::Boolean(i % 2 == 0)))
            .collect();
        let value = BoltValue::Dict(dict);
        let bytes = encode(&value);
        let expected = header(Some(marker::TINY_DICT_NIBBLE), marker::DICT_8, len);
        assert_eq!(&bytes[..expected.len()], &expected[..], "len {len}");
        assert_eq!(decode_all(&bytes).unwrap(), value);
    }
}

/// Decoders must accept non-canonical (wider than necessary) encodings.
#[test]
fn non_canonical_encodings_are_accepted() {
    let cases: &[(&[u8], BoltValue)] = &[
        (&[marker::INT_8, 0x01], BoltValue::Integer(1)),
        (&[marker::INT_16, 0x00, 0x01], BoltValue::Integer(1)),
        (
            &[marker::INT_32, 0x00, 0x00, 0x00, 0x01],
            BoltValue::Integer(1),
        ),
        (
            &[marker::INT_64, 0, 0, 0, 0, 0, 0, 0, 0x01],
            BoltValue::Integer(1),
        ),
        (
            &[marker::STRING_8, 0x01, b'a'],
            BoltValue::String("a".into()),
        ),
        (
            &[marker::STRING_32, 0, 0, 0, 0x01, b'a'],
            BoltValue::String("a".into()),
        ),
        (&[marker::BYTES_16, 0x00, 0x00], BoltValue::Bytes(vec![])),
        (
            &[marker::LIST_16, 0x00, 0x01, 0x05],
            BoltValue::List(vec![BoltValue::Integer(5)]),
        ),
        (
            &[marker::DICT_32, 0, 0, 0, 0x01, 0x81, b'k', 0xC0],
            BoltValue::Dict(BoltDict::from([("k".to_string(), BoltValue::Null)])),
        ),
    ];
    for (bytes, expected) in cases {
        assert_eq!(&decode_all(bytes).unwrap(), expected, "{bytes:02X?}");
    }
}

#[test]
fn special_floats_round_trip_bit_exactly() {
    for f in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -0.0,
        f64::MIN_POSITIVE,
        f64::MAX,
        f64::EPSILON,
    ] {
        let bytes = encode(&BoltValue::Float(f));
        assert_eq!(bytes.len(), 9);
        match decode_all(&bytes).unwrap() {
            BoltValue::Float(back) => assert_eq!(back.to_bits(), f.to_bits(), "{f}"),
            other => panic!("expected float, got {other:?}"),
        }
    }
}

#[test]
fn structure_wire_bytes() {
    let cases: Vec<(BoltValue, Vec<u8>)> = vec![
        (
            BoltValue::Date(BoltDate { days: 1 }),
            vec![0xB1, 0x44, 0x01],
        ),
        (
            BoltValue::Time(BoltTime {
                nanoseconds: 2,
                tz_offset_seconds: 3,
            }),
            vec![0xB2, 0x54, 0x02, 0x03],
        ),
        (
            BoltValue::LocalTime(BoltLocalTime { nanoseconds: 4 }),
            vec![0xB1, 0x74, 0x04],
        ),
        (
            BoltValue::DateTime(BoltDateTime {
                seconds: 1,
                nanoseconds: 2,
                tz_offset_seconds: 3,
            }),
            vec![0xB3, 0x49, 0x01, 0x02, 0x03],
        ),
        (
            BoltValue::DateTimeZoneId(BoltDateTimeZoneId {
                seconds: 1,
                nanoseconds: 2,
                tz_id: "Z".into(),
            }),
            vec![0xB3, 0x69, 0x01, 0x02, 0x81, b'Z'],
        ),
        (
            BoltValue::LocalDateTime(BoltLocalDateTime {
                seconds: 1,
                nanoseconds: 2,
            }),
            vec![0xB2, 0x64, 0x01, 0x02],
        ),
        (
            BoltValue::Duration(BoltDuration {
                months: 1,
                days: 2,
                seconds: 3,
                nanoseconds: 4,
            }),
            vec![0xB4, 0x45, 0x01, 0x02, 0x03, 0x04],
        ),
        (
            BoltValue::Node(BoltNode {
                id: 1,
                labels: vec!["L".into()],
                properties: BoltDict::new(),
                element_id: "e".into(),
            }),
            vec![0xB4, 0x4E, 0x01, 0x91, 0x81, b'L', 0xA0, 0x81, b'e'],
        ),
        (
            BoltValue::Relationship(BoltRelationship {
                id: 1,
                start_node_id: 2,
                end_node_id: 3,
                rel_type: "T".into(),
                properties: BoltDict::new(),
                element_id: "r".into(),
                start_element_id: "s".into(),
                end_element_id: "e".into(),
            }),
            vec![
                0xB8, 0x52, 0x01, 0x02, 0x03, 0x81, b'T', 0xA0, 0x81, b'r', 0x81, b's', 0x81, b'e',
            ],
        ),
        (
            BoltValue::UnboundRelationship(BoltUnboundRelationship {
                id: 1,
                rel_type: "T".into(),
                properties: BoltDict::new(),
                element_id: "u".into(),
            }),
            vec![0xB4, 0x72, 0x01, 0x81, b'T', 0xA0, 0x81, b'u'],
        ),
        (
            BoltValue::Path(BoltPath {
                nodes: vec![],
                rels: vec![],
                indices: vec![],
            }),
            vec![0xB3, 0x50, 0x90, 0x90, 0x90],
        ),
    ];
    for (value, bytes) in cases {
        assert_eq!(&encode(&value)[..], &bytes[..], "{value:?}");
        assert_eq!(decode_all(&bytes).unwrap(), value);
    }

    // Points: srid then IEEE 754 doubles.
    let point = BoltValue::Point2D(BoltPoint2D {
        srid: 7203,
        x: 1.0,
        y: -2.0,
    });
    let mut expected = vec![0xB3, 0x58, marker::INT_16, 0x1C, 0x23, marker::FLOAT_64];
    expected.extend_from_slice(&1.0f64.to_be_bytes());
    expected.push(marker::FLOAT_64);
    expected.extend_from_slice(&(-2.0f64).to_be_bytes());
    assert_eq!(&encode(&point)[..], &expected[..]);
    assert_eq!(decode_all(&expected).unwrap(), point);

    let point3d = BoltValue::Point3D(BoltPoint3D {
        srid: 9157,
        x: 0.5,
        y: 1.5,
        z: 2.5,
    });
    assert_eq!(encode(&point3d)[..2], [0xB4, 0x59]);
    assert_eq!(round_trip(&point3d), point3d);
}

#[test]
fn message_wire_bytes() {
    let mut buf = BytesMut::new();
    encode_client_message(&mut buf, &ClientMessage::Reset);
    assert_eq!(&buf[..], &[0xB0, 0x0F]);

    buf.clear();
    encode_client_message(&mut buf, &ClientMessage::pull_n(1000));
    assert_eq!(
        &buf[..],
        &[0xB1, 0x3F, 0xA1, 0x81, b'n', marker::INT_16, 0x03, 0xE8]
    );

    buf.clear();
    encode_server_message(&mut buf, &ServerMessage::Ignored);
    assert_eq!(&buf[..], &[0xB0, 0x7E]);
}

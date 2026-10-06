//! PackStream decoding: bytes → `BoltValue`.
//!
//! The decoder treats its input as untrusted: every declared size is checked
//! against the bytes actually present before anything is allocated, container
//! pre-allocation is capped, nesting is limited to [`MAX_NESTING_DEPTH`] levels
//! so hostile input cannot overflow the stack, and structure field counts are
//! validated so a malformed structure cannot desynchronise the rest of the
//! message.

use bytes::Buf;

use super::marker;
use crate::error::BoltError;
use crate::types::{
    BoltDate, BoltDateTime, BoltDateTimeZoneId, BoltDict, BoltDuration, BoltLocalDateTime,
    BoltLocalTime, BoltNode, BoltPath, BoltPoint2D, BoltPoint3D, BoltRelationship, BoltTime,
    BoltUnboundRelationship, BoltValue, tag,
};

/// Maximum number of nested containers (lists, dictionaries and structures)
/// the decoder accepts in a single value.
///
/// Decoding is recursive, so without a bound a message made of a few
/// megabytes of `0x91` (a one-element list) bytes would overflow the stack.
/// Real Bolt values rarely nest more than a handful of levels.
pub const MAX_NESTING_DEPTH: usize = 128;

/// Upper bound on the number of list or dictionary slots reserved up front.
///
/// The declared size of a container comes from the network. Reserving it in
/// full would let a short message trigger a huge allocation, so only a small
/// number of slots is reserved and the container grows as elements are
/// actually decoded.
const MAX_PREALLOCATED_ITEMS: usize = 1024;

/// Decodes a single `BoltValue` from the buffer.
///
/// Returns a protocol error (never panics) on truncated input, unknown
/// markers, invalid UTF-8, malformed structures or values nested deeper than
/// [`MAX_NESTING_DEPTH`].
pub fn decode_value(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    decode_nested(buf, 0)
}

fn decode_nested(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    if !buf.has_remaining() {
        return Err(BoltError::Protocol("unexpected end of data".into()));
    }

    let m = buf.get_u8();
    match m {
        // Null
        marker::NULL => Ok(BoltValue::Null),

        // Boolean
        marker::FALSE => Ok(BoltValue::Boolean(false)),
        marker::TRUE => Ok(BoltValue::Boolean(true)),

        // Float
        marker::FLOAT_64 => {
            ensure_remaining(buf, 8)?;
            Ok(BoltValue::Float(buf.get_f64()))
        }

        // Integer markers
        marker::INT_8 => {
            ensure_remaining(buf, 1)?;
            Ok(BoltValue::Integer(i64::from(buf.get_i8())))
        }
        marker::INT_16 => {
            ensure_remaining(buf, 2)?;
            Ok(BoltValue::Integer(i64::from(buf.get_i16())))
        }
        marker::INT_32 => {
            ensure_remaining(buf, 4)?;
            Ok(BoltValue::Integer(i64::from(buf.get_i32())))
        }
        marker::INT_64 => {
            ensure_remaining(buf, 8)?;
            Ok(BoltValue::Integer(buf.get_i64()))
        }

        // Bytes
        marker::BYTES_8 | marker::BYTES_16 | marker::BYTES_32 => {
            let len = read_size(buf, m - marker::BYTES_8)?;
            decode_bytes_data(buf, len)
        }

        // String (longer)
        marker::STRING_8 | marker::STRING_16 | marker::STRING_32 => {
            let len = read_size(buf, m - marker::STRING_8)?;
            decode_string_data(buf, len)
        }

        // List (longer)
        marker::LIST_8 | marker::LIST_16 | marker::LIST_32 => {
            let len = read_size(buf, m - marker::LIST_8)?;
            decode_list_data(buf, len, depth)
        }

        // Dict (longer)
        marker::DICT_8 | marker::DICT_16 | marker::DICT_32 => {
            let len = read_size(buf, m - marker::DICT_8)?;
            decode_dict_data(buf, len, depth)
        }

        // Tiny types and other ranges
        _ => {
            let high = m & 0xF0;
            let low = usize::from(m & 0x0F);

            match high {
                // TINY_STRING: 0x80..=0x8F
                0x80 => decode_string_data(buf, low),

                // TINY_LIST: 0x90..=0x9F
                0x90 => decode_list_data(buf, low, depth),

                // TINY_DICT: 0xA0..=0xAF
                0xA0 => decode_dict_data(buf, low, depth),

                // TINY_STRUCT: 0xB0..=0xBF
                0xB0 => {
                    ensure_remaining(buf, 1)?;
                    let tag_byte = buf.get_u8();
                    decode_struct(buf, tag_byte, low, depth)
                }

                // TINY_INT positive: 0x00..=0x7F
                _ if m <= 0x7F => Ok(BoltValue::Integer(i64::from(m))),

                // TINY_INT negative: 0xF0..=0xFF (-16..-1)
                _ if m >= 0xF0 => Ok(BoltValue::Integer(i64::from(m as i8))),

                _ => Err(BoltError::Protocol(format!(
                    "unknown PackStream marker: 0x{m:02X}"
                ))),
            }
        }
    }
}

/// Reads a big-endian size field. `width_index` is 0, 1 or 2 for the 8, 16
/// and 32-bit marker variants (markers of one family are consecutive).
fn read_size(buf: &mut impl Buf, width_index: u8) -> Result<usize, BoltError> {
    match width_index {
        0 => {
            ensure_remaining(buf, 1)?;
            Ok(usize::from(buf.get_u8()))
        }
        1 => {
            ensure_remaining(buf, 2)?;
            Ok(usize::from(buf.get_u16()))
        }
        _ => {
            ensure_remaining(buf, 4)?;
            usize::try_from(buf.get_u32())
                .map_err(|_| BoltError::Protocol("declared size does not fit in memory".into()))
        }
    }
}

/// Returns the depth for the children of a container at `depth`, or an error
/// when that would exceed [`MAX_NESTING_DEPTH`].
fn enter_container(depth: usize) -> Result<usize, BoltError> {
    let inner = depth + 1;
    if inner > MAX_NESTING_DEPTH {
        Err(BoltError::Protocol(format!(
            "PackStream value nested deeper than {MAX_NESTING_DEPTH} levels"
        )))
    } else {
        Ok(inner)
    }
}

fn ensure_remaining(buf: &impl Buf, needed: usize) -> Result<(), BoltError> {
    if buf.remaining() < needed {
        Err(BoltError::Protocol(format!(
            "need {needed} bytes but only {} remaining",
            buf.remaining()
        )))
    } else {
        Ok(())
    }
}

/// Number of container slots to reserve for a declared size of `len`.
///
/// Every element needs at least `min_bytes_per_item` bytes on the wire, so a
/// declaration larger than that allows is necessarily truncated input. The
/// reservation is further capped at [`MAX_PREALLOCATED_ITEMS`].
fn preallocation(len: usize, remaining: usize, min_bytes_per_item: usize) -> usize {
    len.min(remaining / min_bytes_per_item)
        .min(MAX_PREALLOCATED_ITEMS)
}

fn decode_bytes_data(buf: &mut impl Buf, len: usize) -> Result<BoltValue, BoltError> {
    ensure_remaining(buf, len)?;
    let mut data = vec![0u8; len];
    buf.copy_to_slice(&mut data);
    Ok(BoltValue::Bytes(data))
}

fn decode_string_data(buf: &mut impl Buf, len: usize) -> Result<BoltValue, BoltError> {
    ensure_remaining(buf, len)?;
    let mut data = vec![0u8; len];
    buf.copy_to_slice(&mut data);
    let s = String::from_utf8(data)
        .map_err(|e| BoltError::Protocol(format!("invalid UTF-8 string: {e}")))?;
    Ok(BoltValue::String(s))
}

fn decode_list_data(buf: &mut impl Buf, len: usize, depth: usize) -> Result<BoltValue, BoltError> {
    let inner = enter_container(depth)?;
    // Each list element occupies at least one byte.
    let mut items = Vec::with_capacity(preallocation(len, buf.remaining(), 1));
    for _ in 0..len {
        items.push(decode_nested(buf, inner)?);
    }
    Ok(BoltValue::List(items))
}

fn decode_dict_data(buf: &mut impl Buf, len: usize, depth: usize) -> Result<BoltValue, BoltError> {
    let inner = enter_container(depth)?;
    // Each entry occupies at least two bytes (key marker and value marker).
    let mut dict = BoltDict::with_capacity(preallocation(len, buf.remaining(), 2));
    for _ in 0..len {
        let key = match decode_nested(buf, inner)? {
            BoltValue::String(s) => s,
            other => {
                return Err(BoltError::Protocol(format!(
                    "dictionary key must be a String, got {}",
                    other.type_name()
                )));
            }
        };
        let value = decode_nested(buf, inner)?;
        dict.insert(key, value);
    }
    Ok(BoltValue::Dict(dict))
}

/// Validates the field count of a structure against the counts its tag allows.
fn expect_struct_fields(name: &str, got: usize, allowed: &[usize]) -> Result<(), BoltError> {
    if allowed.contains(&got) {
        Ok(())
    } else {
        Err(BoltError::Protocol(format!(
            "{name} structure expects {allowed:?} fields, got {got}"
        )))
    }
}

fn decode_struct(
    buf: &mut impl Buf,
    tag_byte: u8,
    field_count: usize,
    depth: usize,
) -> Result<BoltValue, BoltError> {
    let inner = enter_container(depth)?;
    match tag_byte {
        tag::NODE => {
            // Node v5: id, labels, properties, element_id (4 fields).
            // Node v4: id, labels, properties (3 fields).
            expect_struct_fields("Node", field_count, &[3, 4])?;
            decode_node(buf, field_count, inner)
        }
        tag::RELATIONSHIP => {
            expect_struct_fields("Relationship", field_count, &[5, 8])?;
            decode_relationship(buf, field_count, inner)
        }
        tag::UNBOUND_RELATIONSHIP => {
            expect_struct_fields("UnboundRelationship", field_count, &[3, 4])?;
            decode_unbound_relationship(buf, field_count, inner)
        }
        tag::PATH => {
            expect_struct_fields("Path", field_count, &[3])?;
            decode_path(buf, inner)
        }
        tag::DATE => {
            expect_struct_fields("Date", field_count, &[1])?;
            decode_date(buf, inner)
        }
        tag::TIME => {
            expect_struct_fields("Time", field_count, &[2])?;
            decode_time(buf, inner)
        }
        tag::LOCAL_TIME => {
            expect_struct_fields("LocalTime", field_count, &[1])?;
            decode_local_time(buf, inner)
        }
        tag::DATE_TIME => {
            expect_struct_fields("DateTime", field_count, &[3])?;
            decode_datetime(buf, inner)
        }
        tag::DATE_TIME_ZONE_ID => {
            expect_struct_fields("DateTimeZoneId", field_count, &[3])?;
            decode_datetime_zone_id(buf, inner)
        }
        tag::LOCAL_DATE_TIME => {
            expect_struct_fields("LocalDateTime", field_count, &[2])?;
            decode_local_datetime(buf, inner)
        }
        tag::DURATION => {
            expect_struct_fields("Duration", field_count, &[4])?;
            decode_duration(buf, inner)
        }
        tag::POINT_2D => {
            expect_struct_fields("Point2D", field_count, &[3])?;
            decode_point2d(buf, inner)
        }
        tag::POINT_3D => {
            expect_struct_fields("Point3D", field_count, &[4])?;
            decode_point3d(buf, inner)
        }
        _ => Err(BoltError::Protocol(format!(
            "unknown struct tag: 0x{tag_byte:02X}"
        ))),
    }
}

// -- Graph structure decoding --

fn decode_node(
    buf: &mut impl Buf,
    field_count: usize,
    depth: usize,
) -> Result<BoltValue, BoltError> {
    let id = require_int(decode_nested(buf, depth)?)?;
    let labels = require_string_list(decode_nested(buf, depth)?)?;
    let properties = require_dict(decode_nested(buf, depth)?)?;
    let element_id = if field_count >= 4 {
        require_string(decode_nested(buf, depth)?)?
    } else {
        id.to_string()
    };
    Ok(BoltValue::Node(BoltNode {
        id,
        labels,
        properties,
        element_id,
    }))
}

fn decode_relationship(
    buf: &mut impl Buf,
    field_count: usize,
    depth: usize,
) -> Result<BoltValue, BoltError> {
    let id = require_int(decode_nested(buf, depth)?)?;
    let start_node_id = require_int(decode_nested(buf, depth)?)?;
    let end_node_id = require_int(decode_nested(buf, depth)?)?;
    let rel_type = require_string(decode_nested(buf, depth)?)?;
    let properties = require_dict(decode_nested(buf, depth)?)?;
    let (element_id, start_element_id, end_element_id) = if field_count >= 8 {
        (
            require_string(decode_nested(buf, depth)?)?,
            require_string(decode_nested(buf, depth)?)?,
            require_string(decode_nested(buf, depth)?)?,
        )
    } else {
        (
            id.to_string(),
            start_node_id.to_string(),
            end_node_id.to_string(),
        )
    };
    Ok(BoltValue::Relationship(BoltRelationship {
        id,
        start_node_id,
        end_node_id,
        rel_type,
        properties,
        element_id,
        start_element_id,
        end_element_id,
    }))
}

fn decode_unbound_relationship(
    buf: &mut impl Buf,
    field_count: usize,
    depth: usize,
) -> Result<BoltValue, BoltError> {
    let id = require_int(decode_nested(buf, depth)?)?;
    let rel_type = require_string(decode_nested(buf, depth)?)?;
    let properties = require_dict(decode_nested(buf, depth)?)?;
    let element_id = if field_count >= 4 {
        require_string(decode_nested(buf, depth)?)?
    } else {
        id.to_string()
    };
    Ok(BoltValue::UnboundRelationship(BoltUnboundRelationship {
        id,
        rel_type,
        properties,
        element_id,
    }))
}

fn decode_path(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let nodes = match decode_nested(buf, depth)? {
        BoltValue::List(items) => items
            .into_iter()
            .map(|v| match v {
                BoltValue::Node(n) => Ok(n),
                other => Err(BoltError::Protocol(format!(
                    "path nodes must be Node, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(BoltError::Protocol("path nodes must be a list".into())),
    };

    let rels = match decode_nested(buf, depth)? {
        BoltValue::List(items) => items
            .into_iter()
            .map(|v| match v {
                BoltValue::UnboundRelationship(r) => Ok(r),
                other => Err(BoltError::Protocol(format!(
                    "path rels must be UnboundRelationship, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(BoltError::Protocol("path rels must be a list".into())),
    };

    let indices = match decode_nested(buf, depth)? {
        BoltValue::List(items) => items
            .into_iter()
            .map(require_int)
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(BoltError::Protocol("path indices must be a list".into())),
    };

    Ok(BoltValue::Path(BoltPath {
        nodes,
        rels,
        indices,
    }))
}

// -- Temporal decoding --

fn decode_date(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let days = require_int(decode_nested(buf, depth)?)?;
    Ok(BoltValue::Date(BoltDate { days }))
}

fn decode_time(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let nanoseconds = require_int(decode_nested(buf, depth)?)?;
    let tz_offset_seconds = require_int(decode_nested(buf, depth)?)?;
    Ok(BoltValue::Time(BoltTime {
        nanoseconds,
        tz_offset_seconds,
    }))
}

fn decode_local_time(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let nanoseconds = require_int(decode_nested(buf, depth)?)?;
    Ok(BoltValue::LocalTime(BoltLocalTime { nanoseconds }))
}

fn decode_datetime(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let seconds = require_int(decode_nested(buf, depth)?)?;
    let nanoseconds = require_int(decode_nested(buf, depth)?)?;
    let tz_offset_seconds = require_int(decode_nested(buf, depth)?)?;
    Ok(BoltValue::DateTime(BoltDateTime {
        seconds,
        nanoseconds,
        tz_offset_seconds,
    }))
}

fn decode_datetime_zone_id(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let seconds = require_int(decode_nested(buf, depth)?)?;
    let nanoseconds = require_int(decode_nested(buf, depth)?)?;
    let tz_id = require_string(decode_nested(buf, depth)?)?;
    Ok(BoltValue::DateTimeZoneId(BoltDateTimeZoneId {
        seconds,
        nanoseconds,
        tz_id,
    }))
}

fn decode_local_datetime(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let seconds = require_int(decode_nested(buf, depth)?)?;
    let nanoseconds = require_int(decode_nested(buf, depth)?)?;
    Ok(BoltValue::LocalDateTime(BoltLocalDateTime {
        seconds,
        nanoseconds,
    }))
}

fn decode_duration(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let months = require_int(decode_nested(buf, depth)?)?;
    let days = require_int(decode_nested(buf, depth)?)?;
    let seconds = require_int(decode_nested(buf, depth)?)?;
    let nanoseconds = require_int(decode_nested(buf, depth)?)?;
    Ok(BoltValue::Duration(BoltDuration {
        months,
        days,
        seconds,
        nanoseconds,
    }))
}

fn decode_point2d(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let srid = require_int(decode_nested(buf, depth)?)?;
    let x = require_float(decode_nested(buf, depth)?)?;
    let y = require_float(decode_nested(buf, depth)?)?;
    Ok(BoltValue::Point2D(BoltPoint2D { srid, x, y }))
}

fn decode_point3d(buf: &mut impl Buf, depth: usize) -> Result<BoltValue, BoltError> {
    let srid = require_int(decode_nested(buf, depth)?)?;
    let x = require_float(decode_nested(buf, depth)?)?;
    let y = require_float(decode_nested(buf, depth)?)?;
    let z = require_float(decode_nested(buf, depth)?)?;
    Ok(BoltValue::Point3D(BoltPoint3D { srid, x, y, z }))
}

// -- Value extraction helpers --

fn require_int(v: BoltValue) -> Result<i64, BoltError> {
    match v {
        BoltValue::Integer(i) => Ok(i),
        other => Err(BoltError::Protocol(format!(
            "expected Integer, got {}",
            other.type_name()
        ))),
    }
}

fn require_float(v: BoltValue) -> Result<f64, BoltError> {
    match v {
        BoltValue::Float(f) => Ok(f),
        other => Err(BoltError::Protocol(format!(
            "expected Float, got {}",
            other.type_name()
        ))),
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

fn require_string_list(v: BoltValue) -> Result<Vec<String>, BoltError> {
    match v {
        BoltValue::List(items) => items.into_iter().map(require_string).collect(),
        other => Err(BoltError::Protocol(format!(
            "expected List of String, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packstream::encode;
    use bytes::BytesMut;

    /// Encode then decode a value and verify round-trip.
    fn round_trip(value: &BoltValue) -> BoltValue {
        let mut buf = BytesMut::new();
        encode::encode_value(&mut buf, value);
        let mut cursor = &buf[..];
        decode_value(&mut cursor).expect("decode failed")
    }

    #[test]
    fn round_trip_null() {
        assert_eq!(round_trip(&BoltValue::Null), BoltValue::Null);
    }

    #[test]
    fn round_trip_bool() {
        assert_eq!(
            round_trip(&BoltValue::Boolean(true)),
            BoltValue::Boolean(true)
        );
        assert_eq!(
            round_trip(&BoltValue::Boolean(false)),
            BoltValue::Boolean(false)
        );
    }

    #[test]
    fn round_trip_integers() {
        // TINY_INT boundaries
        for i in [-16, -1, 0, 1, 42, 127] {
            assert_eq!(
                round_trip(&BoltValue::Integer(i)),
                BoltValue::Integer(i),
                "failed for {i}"
            );
        }
        // INT_8
        for i in [-128, -17] {
            assert_eq!(
                round_trip(&BoltValue::Integer(i)),
                BoltValue::Integer(i),
                "failed for {i}"
            );
        }
        // INT_16
        for i in [-129, 128, -32768, 32767] {
            assert_eq!(
                round_trip(&BoltValue::Integer(i)),
                BoltValue::Integer(i),
                "failed for {i}"
            );
        }
        // INT_32
        for i in [-32769, 32768, i64::from(i32::MIN), i64::from(i32::MAX)] {
            assert_eq!(
                round_trip(&BoltValue::Integer(i)),
                BoltValue::Integer(i),
                "failed for {i}"
            );
        }
        // INT_64
        for i in [
            i64::from(i32::MAX) + 1,
            i64::from(i32::MIN) - 1,
            i64::MAX,
            i64::MIN,
        ] {
            assert_eq!(
                round_trip(&BoltValue::Integer(i)),
                BoltValue::Integer(i),
                "failed for {i}"
            );
        }
    }

    #[test]
    fn round_trip_float() {
        let val = BoltValue::Float(std::f64::consts::PI);
        assert_eq!(round_trip(&val), val);
    }

    #[test]
    fn round_trip_strings() {
        // Empty
        assert_eq!(
            round_trip(&BoltValue::String(String::new())),
            BoltValue::String(String::new()),
        );
        // Tiny (1..15 bytes)
        assert_eq!(
            round_trip(&BoltValue::String("hello".into())),
            BoltValue::String("hello".into()),
        );
        // STRING_8 (16+ bytes)
        let s: String = "a".repeat(200);
        assert_eq!(
            round_trip(&BoltValue::String(s.clone())),
            BoltValue::String(s),
        );
    }

    #[test]
    fn round_trip_bytes() {
        let val = BoltValue::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(round_trip(&val), val);
    }

    #[test]
    fn round_trip_list() {
        let val = BoltValue::List(vec![
            BoltValue::Integer(1),
            BoltValue::String("two".into()),
            BoltValue::Boolean(true),
        ]);
        assert_eq!(round_trip(&val), val);
    }

    #[test]
    fn round_trip_dict() {
        let val = BoltValue::Dict(BoltDict::from([
            ("name".to_string(), BoltValue::String("Alice".into())),
            ("age".to_string(), BoltValue::Integer(30)),
        ]));
        assert_eq!(round_trip(&val), val);
    }

    #[test]
    fn round_trip_node() {
        let node = BoltNode {
            id: 42,
            labels: vec!["Person".into()],
            properties: BoltDict::from([("name".to_string(), BoltValue::String("Alice".into()))]),
            element_id: "42".into(),
        };
        assert_eq!(
            round_trip(&BoltValue::Node(node.clone())),
            BoltValue::Node(node)
        );
    }

    #[test]
    fn round_trip_date() {
        let val = BoltValue::Date(BoltDate { days: 19000 });
        assert_eq!(round_trip(&val), val);
    }

    #[test]
    fn round_trip_duration() {
        let val = BoltValue::Duration(BoltDuration {
            months: 12,
            days: 30,
            seconds: 3600,
            nanoseconds: 500,
        });
        assert_eq!(round_trip(&val), val);
    }

    #[test]
    fn round_trip_point2d() {
        let val = BoltValue::Point2D(BoltPoint2D {
            srid: 4326,
            x: 12.5,
            y: 55.7,
        });
        assert_eq!(round_trip(&val), val);
    }

    fn decode_bytes(bytes: &[u8]) -> Result<BoltValue, BoltError> {
        let mut cursor = bytes;
        decode_value(&mut cursor)
    }

    fn protocol_error(bytes: &[u8]) -> String {
        match decode_bytes(bytes) {
            Err(BoltError::Protocol(msg)) => msg,
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    /// Regression: decoding used unbounded recursion, so a message of a few
    /// hundred kilobytes of `0x91` (a one-element list) overflowed the stack
    /// and aborted the whole process.
    #[test]
    fn deeply_nested_lists_are_rejected_without_stack_overflow() {
        let mut bytes = vec![0x91; 1_000_000];
        bytes.push(0xC0);
        let msg = protocol_error(&bytes);
        assert!(msg.contains("nested deeper"), "unexpected error: {msg}");
    }

    #[test]
    fn deeply_nested_dicts_and_structs_are_rejected() {
        // {"": {"": {"": ...}}}
        let mut dicts = Vec::new();
        for _ in 0..10_000 {
            dicts.extend_from_slice(&[0xA1, 0x80]);
        }
        dicts.push(0xC0);
        assert!(protocol_error(&dicts).contains("nested deeper"));

        // Path structures whose node list nests another path forever.
        let mut structs = Vec::new();
        for _ in 0..10_000 {
            structs.extend_from_slice(&[0xB3, tag::PATH, 0x91]);
        }
        assert!(protocol_error(&structs).contains("nested deeper"));
    }

    #[test]
    fn nesting_exactly_at_the_limit_is_accepted() {
        let mut bytes = vec![0x91; MAX_NESTING_DEPTH];
        bytes.push(0x01);
        let mut value = decode_bytes(&bytes).expect("decode at limit");
        for _ in 0..MAX_NESTING_DEPTH {
            value = match value {
                BoltValue::List(mut items) => items.pop().expect("one item"),
                other => panic!("expected list, got {other:?}"),
            };
        }
        assert_eq!(value, BoltValue::Integer(1));

        let mut too_deep = vec![0x91; MAX_NESTING_DEPTH + 1];
        too_deep.push(0x01);
        assert!(decode_bytes(&too_deep).is_err());
    }

    /// Regression: the declared size of a container used to be reserved up
    /// front (bounded only by the message size), so a list declaring four
    /// billion elements reserved gigabytes before the first element failed.
    #[test]
    fn huge_declared_sizes_fail_fast() {
        // LIST_32 / DICT_32 / STRING_32 / BYTES_32 declaring u32::MAX entries.
        for m in [
            marker::LIST_32,
            marker::DICT_32,
            marker::STRING_32,
            marker::BYTES_32,
        ] {
            let bytes = [m, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
            assert!(decode_bytes(&bytes).is_err(), "marker 0x{m:02X}");
        }
    }

    #[test]
    fn preallocation_is_capped() {
        assert_eq!(preallocation(u32::MAX as usize, 16 << 20, 1), 1024);
        assert_eq!(preallocation(10, 16 << 20, 1), 10);
        assert_eq!(preallocation(100, 3, 1), 3);
        assert_eq!(preallocation(100, 7, 2), 3);
    }

    #[test]
    fn truncated_input_is_an_error_for_every_marker() {
        let truncated: &[&[u8]] = &[
            &[],
            &[marker::FLOAT_64, 0x00],
            &[marker::INT_8],
            &[marker::INT_16, 0x00],
            &[marker::INT_32, 0x00, 0x00],
            &[marker::INT_64, 0x00, 0x00, 0x00],
            &[marker::BYTES_8],
            &[marker::BYTES_8, 0x02, 0x00],
            &[marker::BYTES_16, 0x00],
            &[marker::BYTES_32, 0x00, 0x00],
            &[0x85, b'a', b'b'],
            &[marker::STRING_8, 0x10],
            &[marker::STRING_16, 0x00],
            &[marker::STRING_32, 0x00, 0x00, 0x00],
            &[0x92, 0x01],
            &[marker::LIST_8],
            &[marker::LIST_16, 0x00],
            &[marker::LIST_32, 0x00, 0x00, 0x00, 0x01],
            &[0xA1, 0x81, b'k'],
            &[marker::DICT_8, 0x01, 0x81, b'k'],
            &[marker::DICT_16],
            &[marker::DICT_32, 0x00, 0x00, 0x00],
            &[0xB1],
            &[0xB1, tag::DATE],
            &[0xB3, tag::POINT_2D, 0x01, marker::FLOAT_64],
        ];
        for bytes in truncated {
            assert!(
                decode_bytes(bytes).is_err(),
                "decoded truncated {bytes:02X?}"
            );
        }
    }

    #[test]
    fn reserved_markers_are_rejected() {
        for m in [
            0xC4u8, 0xC5, 0xC6, 0xC7, 0xCF, 0xD3, 0xD7, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF, 0xE0, 0xEF,
        ] {
            let msg = protocol_error(&[m, 0x00, 0x00, 0x00, 0x00]);
            assert!(
                msg.contains("unknown PackStream marker"),
                "0x{m:02X}: {msg}"
            );
        }
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let msg = protocol_error(&[0x82, 0xC3, 0x28]);
        assert!(msg.contains("UTF-8"), "{msg}");
    }

    /// Regression: structure field counts were ignored, so a `Date` declared
    /// with zero fields silently consumed the next value of the message.
    #[test]
    fn struct_field_count_mismatch_is_rejected() {
        // Date with 0 fields followed by an integer that must not be consumed.
        let msg = protocol_error(&[0xB0, tag::DATE, 0x05]);
        assert!(msg.contains("Date structure expects"), "{msg}");

        // Duration with 2 fields.
        let msg = protocol_error(&[0xB2, tag::DURATION, 0x01, 0x02]);
        assert!(msg.contains("Duration"), "{msg}");

        // Node with 5 fields: id, labels, props, element_id, extra.
        let msg = protocol_error(&[0xB5, tag::NODE, 0x01, 0x90, 0xA0, 0x81, b'1', 0x01]);
        assert!(msg.contains("Node"), "{msg}");

        // Path with 2 fields.
        let msg = protocol_error(&[0xB2, tag::PATH, 0x90, 0x90]);
        assert!(msg.contains("Path"), "{msg}");
    }

    #[test]
    fn legacy_field_counts_are_still_accepted() {
        // Bolt 4 Node: id, labels, properties.
        let node = decode_bytes(&[0xB3, tag::NODE, 0x07, 0x91, 0x81, b'L', 0xA0]).unwrap();
        assert_eq!(
            node,
            BoltValue::Node(BoltNode {
                id: 7,
                labels: vec!["L".into()],
                properties: BoltDict::new(),
                element_id: "7".into(),
            })
        );

        // Bolt 4 Relationship: id, start, end, type, properties.
        let rel =
            decode_bytes(&[0xB5, tag::RELATIONSHIP, 0x01, 0x02, 0x03, 0x81, b'T', 0xA0]).unwrap();
        assert_eq!(
            rel,
            BoltValue::Relationship(BoltRelationship {
                id: 1,
                start_node_id: 2,
                end_node_id: 3,
                rel_type: "T".into(),
                properties: BoltDict::new(),
                element_id: "1".into(),
                start_element_id: "2".into(),
                end_element_id: "3".into(),
            })
        );

        // Bolt 4 UnboundRelationship: id, type, properties.
        let unbound =
            decode_bytes(&[0xB3, tag::UNBOUND_RELATIONSHIP, 0x04, 0x81, b'T', 0xA0]).unwrap();
        assert!(matches!(unbound, BoltValue::UnboundRelationship(r) if r.element_id == "4"));
    }

    #[test]
    fn unknown_struct_tag_is_rejected() {
        let msg = protocol_error(&[0xB1, 0x46, 0x01]);
        assert!(msg.contains("unknown struct tag: 0x46"), "{msg}");
    }

    #[test]
    fn struct_fields_with_wrong_types_are_rejected() {
        // Date whose day count is a string.
        assert!(decode_bytes(&[0xB1, tag::DATE, 0x81, b'x']).is_err());
        // Point2D with integer coordinates.
        assert!(decode_bytes(&[0xB3, tag::POINT_2D, 0x01, 0x02, 0x03]).is_err());
        // Path whose node list contains an integer.
        assert!(decode_bytes(&[0xB3, tag::PATH, 0x91, 0x01, 0x90, 0x90]).is_err());
        // Node whose labels are not strings.
        assert!(decode_bytes(&[0xB4, tag::NODE, 0x01, 0x91, 0x01, 0xA0, 0x80]).is_err());
    }

    /// Regression: errors embedded the offending value via `Display`, so a
    /// dictionary key that was a multi-megabyte list produced a
    /// multi-megabyte error string (echoed back to the client in FAILURE).
    #[test]
    fn error_messages_do_not_echo_value_contents() {
        let mut bytes = vec![0xA1, marker::LIST_16, 0x10, 0x00];
        bytes.extend(std::iter::repeat_n(0x7F, 0x1000));
        bytes.push(0xC0);
        let msg = protocol_error(&bytes);
        assert!(msg.contains("got List"), "{msg}");
        assert!(
            msg.len() < 100,
            "error message too long: {} bytes",
            msg.len()
        );
    }
}

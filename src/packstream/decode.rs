//! PackStream decoding: bytes → `BoltValue`.

use bytes::Buf;

use super::marker;
use crate::error::BoltError;
use crate::types::{
    tag, BoltDate, BoltDateTime, BoltDateTimeZoneId, BoltDict, BoltDuration, BoltLocalDateTime,
    BoltLocalTime, BoltNode, BoltPath, BoltPoint2D, BoltPoint3D, BoltRelationship, BoltTime,
    BoltUnboundRelationship, BoltValue,
};

/// Decodes a single `BoltValue` from the buffer.
pub fn decode_value(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
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
        marker::BYTES_8 => {
            ensure_remaining(buf, 1)?;
            let len = buf.get_u8() as usize;
            decode_bytes_data(buf, len)
        }
        marker::BYTES_16 => {
            ensure_remaining(buf, 2)?;
            let len = buf.get_u16() as usize;
            decode_bytes_data(buf, len)
        }
        marker::BYTES_32 => {
            ensure_remaining(buf, 4)?;
            let len = buf.get_u32() as usize;
            decode_bytes_data(buf, len)
        }

        // String (longer)
        marker::STRING_8 => {
            ensure_remaining(buf, 1)?;
            let len = buf.get_u8() as usize;
            decode_string_data(buf, len)
        }
        marker::STRING_16 => {
            ensure_remaining(buf, 2)?;
            let len = buf.get_u16() as usize;
            decode_string_data(buf, len)
        }
        marker::STRING_32 => {
            ensure_remaining(buf, 4)?;
            let len = buf.get_u32() as usize;
            decode_string_data(buf, len)
        }

        // List (longer)
        marker::LIST_8 => {
            ensure_remaining(buf, 1)?;
            let len = buf.get_u8() as usize;
            decode_list_data(buf, len)
        }
        marker::LIST_16 => {
            ensure_remaining(buf, 2)?;
            let len = buf.get_u16() as usize;
            decode_list_data(buf, len)
        }
        marker::LIST_32 => {
            ensure_remaining(buf, 4)?;
            let len = buf.get_u32() as usize;
            decode_list_data(buf, len)
        }

        // Dict (longer)
        marker::DICT_8 => {
            ensure_remaining(buf, 1)?;
            let len = buf.get_u8() as usize;
            decode_dict_data(buf, len)
        }
        marker::DICT_16 => {
            ensure_remaining(buf, 2)?;
            let len = buf.get_u16() as usize;
            decode_dict_data(buf, len)
        }
        marker::DICT_32 => {
            ensure_remaining(buf, 4)?;
            let len = buf.get_u32() as usize;
            decode_dict_data(buf, len)
        }

        // Tiny types and other ranges
        _ => {
            let high = m & 0xF0;
            let low = m & 0x0F;

            match high {
                // TINY_STRING: 0x80..=0x8F
                0x80 => decode_string_data(buf, low as usize),

                // TINY_LIST: 0x90..=0x9F
                0x90 => decode_list_data(buf, low as usize),

                // TINY_DICT: 0xA0..=0xAF
                0xA0 => decode_dict_data(buf, low as usize),

                // TINY_STRUCT: 0xB0..=0xBF
                0xB0 => {
                    ensure_remaining(buf, 1)?;
                    let tag_byte = buf.get_u8();
                    decode_struct(buf, tag_byte, low as usize)
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

fn decode_list_data(buf: &mut impl Buf, len: usize) -> Result<BoltValue, BoltError> {
    let mut items = Vec::with_capacity(len);
    for _ in 0..len {
        items.push(decode_value(buf)?);
    }
    Ok(BoltValue::List(items))
}

fn decode_dict_data(buf: &mut impl Buf, len: usize) -> Result<BoltValue, BoltError> {
    let mut dict = BoltDict::with_capacity(len);
    for _ in 0..len {
        let key = match decode_value(buf)? {
            BoltValue::String(s) => s,
            other => {
                return Err(BoltError::Protocol(format!(
                    "dict key must be a string, got: {other}"
                )));
            }
        };
        let value = decode_value(buf)?;
        dict.insert(key, value);
    }
    Ok(BoltValue::Dict(dict))
}

fn decode_struct(
    buf: &mut impl Buf,
    tag_byte: u8,
    field_count: usize,
) -> Result<BoltValue, BoltError> {
    match tag_byte {
        tag::NODE => decode_node(buf, field_count),
        tag::RELATIONSHIP => decode_relationship(buf, field_count),
        tag::UNBOUND_RELATIONSHIP => decode_unbound_relationship(buf, field_count),
        tag::PATH => decode_path(buf, field_count),
        tag::DATE => decode_date(buf),
        tag::TIME => decode_time(buf),
        tag::LOCAL_TIME => decode_local_time(buf),
        tag::DATE_TIME => decode_datetime(buf),
        tag::DATE_TIME_ZONE_ID => decode_datetime_zone_id(buf),
        tag::LOCAL_DATE_TIME => decode_local_datetime(buf),
        tag::DURATION => decode_duration(buf),
        tag::POINT_2D => decode_point2d(buf),
        tag::POINT_3D => decode_point3d(buf),
        _ => {
            // Unknown struct: skip fields
            for _ in 0..field_count {
                decode_value(buf)?;
            }
            Err(BoltError::Protocol(format!(
                "unknown struct tag: 0x{tag_byte:02X}"
            )))
        }
    }
}

// -- Graph structure decoding --

fn decode_node(buf: &mut impl Buf, field_count: usize) -> Result<BoltValue, BoltError> {
    // Node v5: id, labels, properties, element_id (4 fields)
    // Node v4: id, labels, properties (3 fields)
    let id = require_int(decode_value(buf)?)?;
    let labels = require_string_list(decode_value(buf)?)?;
    let properties = require_dict(decode_value(buf)?)?;
    let element_id = if field_count >= 4 {
        require_string(decode_value(buf)?)?
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

fn decode_relationship(buf: &mut impl Buf, field_count: usize) -> Result<BoltValue, BoltError> {
    let id = require_int(decode_value(buf)?)?;
    let start_node_id = require_int(decode_value(buf)?)?;
    let end_node_id = require_int(decode_value(buf)?)?;
    let rel_type = require_string(decode_value(buf)?)?;
    let properties = require_dict(decode_value(buf)?)?;
    let (element_id, start_element_id, end_element_id) = if field_count >= 8 {
        (
            require_string(decode_value(buf)?)?,
            require_string(decode_value(buf)?)?,
            require_string(decode_value(buf)?)?,
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
) -> Result<BoltValue, BoltError> {
    let id = require_int(decode_value(buf)?)?;
    let rel_type = require_string(decode_value(buf)?)?;
    let properties = require_dict(decode_value(buf)?)?;
    let element_id = if field_count >= 4 {
        require_string(decode_value(buf)?)?
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

fn decode_path(buf: &mut impl Buf, _field_count: usize) -> Result<BoltValue, BoltError> {
    let nodes_val = decode_value(buf)?;
    let nodes = match nodes_val {
        BoltValue::List(items) => items
            .into_iter()
            .map(|v| match v {
                BoltValue::Node(n) => Ok(n),
                other => Err(BoltError::Protocol(format!(
                    "path nodes must be Node, got: {other}"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(BoltError::Protocol("path nodes must be a list".into())),
    };

    let rels_val = decode_value(buf)?;
    let rels = match rels_val {
        BoltValue::List(items) => items
            .into_iter()
            .map(|v| match v {
                BoltValue::UnboundRelationship(r) => Ok(r),
                other => Err(BoltError::Protocol(format!(
                    "path rels must be UnboundRelationship, got: {other}"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(BoltError::Protocol("path rels must be a list".into())),
    };

    let indices_val = decode_value(buf)?;
    let indices = match indices_val {
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

fn decode_date(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let days = require_int(decode_value(buf)?)?;
    Ok(BoltValue::Date(BoltDate { days }))
}

fn decode_time(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let nanoseconds = require_int(decode_value(buf)?)?;
    let tz_offset_seconds = require_int(decode_value(buf)?)?;
    Ok(BoltValue::Time(BoltTime {
        nanoseconds,
        tz_offset_seconds,
    }))
}

fn decode_local_time(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let nanoseconds = require_int(decode_value(buf)?)?;
    Ok(BoltValue::LocalTime(BoltLocalTime { nanoseconds }))
}

fn decode_datetime(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let seconds = require_int(decode_value(buf)?)?;
    let nanoseconds = require_int(decode_value(buf)?)?;
    let tz_offset_seconds = require_int(decode_value(buf)?)?;
    Ok(BoltValue::DateTime(BoltDateTime {
        seconds,
        nanoseconds,
        tz_offset_seconds,
    }))
}

fn decode_datetime_zone_id(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let seconds = require_int(decode_value(buf)?)?;
    let nanoseconds = require_int(decode_value(buf)?)?;
    let tz_id = require_string(decode_value(buf)?)?;
    Ok(BoltValue::DateTimeZoneId(BoltDateTimeZoneId {
        seconds,
        nanoseconds,
        tz_id,
    }))
}

fn decode_local_datetime(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let seconds = require_int(decode_value(buf)?)?;
    let nanoseconds = require_int(decode_value(buf)?)?;
    Ok(BoltValue::LocalDateTime(BoltLocalDateTime {
        seconds,
        nanoseconds,
    }))
}

fn decode_duration(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let months = require_int(decode_value(buf)?)?;
    let days = require_int(decode_value(buf)?)?;
    let seconds = require_int(decode_value(buf)?)?;
    let nanoseconds = require_int(decode_value(buf)?)?;
    Ok(BoltValue::Duration(BoltDuration {
        months,
        days,
        seconds,
        nanoseconds,
    }))
}

fn decode_point2d(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let srid = require_int(decode_value(buf)?)?;
    let x = require_float(decode_value(buf)?)?;
    let y = require_float(decode_value(buf)?)?;
    Ok(BoltValue::Point2D(BoltPoint2D { srid, x, y }))
}

fn decode_point3d(buf: &mut impl Buf) -> Result<BoltValue, BoltError> {
    let srid = require_int(decode_value(buf)?)?;
    let x = require_float(decode_value(buf)?)?;
    let y = require_float(decode_value(buf)?)?;
    let z = require_float(decode_value(buf)?)?;
    Ok(BoltValue::Point3D(BoltPoint3D { srid, x, y, z }))
}

// -- Value extraction helpers --

fn require_int(v: BoltValue) -> Result<i64, BoltError> {
    match v {
        BoltValue::Integer(i) => Ok(i),
        other => Err(BoltError::Protocol(format!("expected int, got: {other}"))),
    }
}

fn require_float(v: BoltValue) -> Result<f64, BoltError> {
    match v {
        BoltValue::Float(f) => Ok(f),
        other => Err(BoltError::Protocol(format!("expected float, got: {other}"))),
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

fn require_string_list(v: BoltValue) -> Result<Vec<String>, BoltError> {
    match v {
        BoltValue::List(items) => items.into_iter().map(require_string).collect(),
        other => Err(BoltError::Protocol(format!(
            "expected string list, got: {other}"
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
        assert_eq!(round_trip(&BoltValue::Boolean(true)), BoltValue::Boolean(true));
        assert_eq!(round_trip(&BoltValue::Boolean(false)), BoltValue::Boolean(false));
    }

    #[test]
    fn round_trip_integers() {
        // TINY_INT boundaries
        for i in [-16, -1, 0, 1, 42, 127] {
            assert_eq!(round_trip(&BoltValue::Integer(i)), BoltValue::Integer(i), "failed for {i}");
        }
        // INT_8
        for i in [-128, -17] {
            assert_eq!(round_trip(&BoltValue::Integer(i)), BoltValue::Integer(i), "failed for {i}");
        }
        // INT_16
        for i in [-129, 128, -32768, 32767] {
            assert_eq!(round_trip(&BoltValue::Integer(i)), BoltValue::Integer(i), "failed for {i}");
        }
        // INT_32
        for i in [-32769, 32768, i64::from(i32::MIN), i64::from(i32::MAX)] {
            assert_eq!(round_trip(&BoltValue::Integer(i)), BoltValue::Integer(i), "failed for {i}");
        }
        // INT_64
        for i in [i64::from(i32::MAX) + 1, i64::from(i32::MIN) - 1, i64::MAX, i64::MIN] {
            assert_eq!(round_trip(&BoltValue::Integer(i)), BoltValue::Integer(i), "failed for {i}");
        }
    }

    #[test]
    fn round_trip_float() {
        let val = BoltValue::Float(3.14159);
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
            properties: BoltDict::from([
                ("name".to_string(), BoltValue::String("Alice".into())),
            ]),
            element_id: "42".into(),
        };
        assert_eq!(round_trip(&BoltValue::Node(node.clone())), BoltValue::Node(node));
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
}

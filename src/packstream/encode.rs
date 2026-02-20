//! PackStream encoding: `BoltValue` → bytes.

use bytes::{BufMut, BytesMut};

use super::marker;
use crate::types::{tag, BoltValue};

/// Encodes a `BoltValue` into the buffer using PackStream format.
pub fn encode_value(buf: &mut BytesMut, value: &BoltValue) {
    match value {
        BoltValue::Null => encode_null(buf),
        BoltValue::Boolean(b) => encode_bool(buf, *b),
        BoltValue::Integer(i) => encode_int(buf, *i),
        BoltValue::Float(f) => encode_float(buf, *f),
        BoltValue::String(s) => encode_string(buf, s),
        BoltValue::Bytes(b) => encode_bytes(buf, b),
        BoltValue::List(items) => encode_list(buf, items),
        BoltValue::Dict(dict) => encode_dict(buf, dict),
        BoltValue::Node(n) => encode_node(buf, n),
        BoltValue::Relationship(r) => encode_relationship(buf, r),
        BoltValue::UnboundRelationship(r) => encode_unbound_relationship(buf, r),
        BoltValue::Path(p) => encode_path(buf, p),
        BoltValue::Date(d) => encode_date(buf, d),
        BoltValue::Time(t) => encode_time(buf, t),
        BoltValue::LocalTime(t) => encode_local_time(buf, t),
        BoltValue::DateTime(dt) => encode_datetime(buf, dt),
        BoltValue::DateTimeZoneId(dt) => encode_datetime_zone_id(buf, dt),
        BoltValue::LocalDateTime(dt) => encode_local_datetime(buf, dt),
        BoltValue::Duration(d) => encode_duration(buf, d),
        BoltValue::Point2D(p) => encode_point2d(buf, p),
        BoltValue::Point3D(p) => encode_point3d(buf, p),
    }
}

pub fn encode_null(buf: &mut BytesMut) {
    buf.put_u8(marker::NULL);
}

pub fn encode_bool(buf: &mut BytesMut, value: bool) {
    buf.put_u8(if value { marker::TRUE } else { marker::FALSE });
}

/// Encodes an integer using the smallest possible PackStream representation.
pub fn encode_int(buf: &mut BytesMut, value: i64) {
    if (-16..=127).contains(&value) {
        // TINY_INT: single byte
        buf.put_u8(value as u8);
    } else if i64::from(i8::MIN) <= value && value <= i64::from(i8::MAX) {
        buf.put_u8(marker::INT_8);
        buf.put_i8(value as i8);
    } else if i64::from(i16::MIN) <= value && value <= i64::from(i16::MAX) {
        buf.put_u8(marker::INT_16);
        buf.put_i16(value as i16);
    } else if i64::from(i32::MIN) <= value && value <= i64::from(i32::MAX) {
        buf.put_u8(marker::INT_32);
        buf.put_i32(value as i32);
    } else {
        buf.put_u8(marker::INT_64);
        buf.put_i64(value);
    }
}

pub fn encode_float(buf: &mut BytesMut, value: f64) {
    buf.put_u8(marker::FLOAT_64);
    buf.put_f64(value);
}

/// Encodes a string (size = byte length, not char count).
pub fn encode_string(buf: &mut BytesMut, value: &str) {
    let len = value.len();
    encode_string_header(buf, len);
    buf.put_slice(value.as_bytes());
}

fn encode_string_header(buf: &mut BytesMut, len: usize) {
    if len <= 15 {
        buf.put_u8(marker::TINY_STRING_NIBBLE | len as u8);
    } else if len <= 255 {
        buf.put_u8(marker::STRING_8);
        buf.put_u8(len as u8);
    } else if len <= 65535 {
        buf.put_u8(marker::STRING_16);
        buf.put_u16(len as u16);
    } else {
        buf.put_u8(marker::STRING_32);
        buf.put_u32(len as u32);
    }
}

pub fn encode_bytes(buf: &mut BytesMut, value: &[u8]) {
    let len = value.len();
    if len <= 255 {
        buf.put_u8(marker::BYTES_8);
        buf.put_u8(len as u8);
    } else if len <= 65535 {
        buf.put_u8(marker::BYTES_16);
        buf.put_u16(len as u16);
    } else {
        buf.put_u8(marker::BYTES_32);
        buf.put_u32(len as u32);
    }
    buf.put_slice(value);
}

pub fn encode_list(buf: &mut BytesMut, items: &[BoltValue]) {
    let len = items.len();
    encode_list_header(buf, len);
    for item in items {
        encode_value(buf, item);
    }
}

fn encode_list_header(buf: &mut BytesMut, len: usize) {
    if len <= 15 {
        buf.put_u8(marker::TINY_LIST_NIBBLE | len as u8);
    } else if len <= 255 {
        buf.put_u8(marker::LIST_8);
        buf.put_u8(len as u8);
    } else if len <= 65535 {
        buf.put_u8(marker::LIST_16);
        buf.put_u16(len as u16);
    } else {
        buf.put_u8(marker::LIST_32);
        buf.put_u32(len as u32);
    }
}

pub fn encode_dict(
    buf: &mut BytesMut,
    dict: &std::collections::HashMap<String, BoltValue>,
) {
    let len = dict.len();
    encode_dict_header(buf, len);
    for (key, value) in dict {
        encode_string(buf, key);
        encode_value(buf, value);
    }
}

fn encode_dict_header(buf: &mut BytesMut, len: usize) {
    if len <= 15 {
        buf.put_u8(marker::TINY_DICT_NIBBLE | len as u8);
    } else if len <= 255 {
        buf.put_u8(marker::DICT_8);
        buf.put_u8(len as u8);
    } else if len <= 65535 {
        buf.put_u8(marker::DICT_16);
        buf.put_u16(len as u16);
    } else {
        buf.put_u8(marker::DICT_32);
        buf.put_u32(len as u32);
    }
}

/// Encodes a structure header: marker byte (0xBn) + tag byte.
pub fn encode_struct_header(buf: &mut BytesMut, tag_byte: u8, field_count: usize) {
    debug_assert!(field_count <= 15, "struct field count must be <= 15");
    buf.put_u8(marker::TINY_STRUCT_NIBBLE | field_count as u8);
    buf.put_u8(tag_byte);
}

// -- Graph structure encoding --

fn encode_node(buf: &mut BytesMut, n: &crate::types::BoltNode) {
    // Node: tag 0x4E, 4 fields: id, labels, properties, element_id
    encode_struct_header(buf, tag::NODE, 4);
    encode_int(buf, n.id);
    encode_list_header(buf, n.labels.len());
    for label in &n.labels {
        encode_string(buf, label);
    }
    encode_dict(buf, &n.properties);
    encode_string(buf, &n.element_id);
}

fn encode_relationship(buf: &mut BytesMut, r: &crate::types::BoltRelationship) {
    // Relationship: tag 0x52, 8 fields
    encode_struct_header(buf, tag::RELATIONSHIP, 8);
    encode_int(buf, r.id);
    encode_int(buf, r.start_node_id);
    encode_int(buf, r.end_node_id);
    encode_string(buf, &r.rel_type);
    encode_dict(buf, &r.properties);
    encode_string(buf, &r.element_id);
    encode_string(buf, &r.start_element_id);
    encode_string(buf, &r.end_element_id);
}

fn encode_unbound_relationship(buf: &mut BytesMut, r: &crate::types::BoltUnboundRelationship) {
    // UnboundRelationship: tag 0x72, 4 fields
    encode_struct_header(buf, tag::UNBOUND_RELATIONSHIP, 4);
    encode_int(buf, r.id);
    encode_string(buf, &r.rel_type);
    encode_dict(buf, &r.properties);
    encode_string(buf, &r.element_id);
}

fn encode_path(buf: &mut BytesMut, p: &crate::types::BoltPath) {
    // Path: tag 0x50, 3 fields: nodes, rels, indices
    encode_struct_header(buf, tag::PATH, 3);
    encode_list_header(buf, p.nodes.len());
    for node in &p.nodes {
        encode_node(buf, node);
    }
    encode_list_header(buf, p.rels.len());
    for rel in &p.rels {
        encode_unbound_relationship(buf, rel);
    }
    encode_list_header(buf, p.indices.len());
    for &idx in &p.indices {
        encode_int(buf, idx);
    }
}

// -- Temporal structure encoding --

fn encode_date(buf: &mut BytesMut, d: &crate::types::BoltDate) {
    encode_struct_header(buf, tag::DATE, 1);
    encode_int(buf, d.days);
}

fn encode_time(buf: &mut BytesMut, t: &crate::types::BoltTime) {
    encode_struct_header(buf, tag::TIME, 2);
    encode_int(buf, t.nanoseconds);
    encode_int(buf, t.tz_offset_seconds);
}

fn encode_local_time(buf: &mut BytesMut, t: &crate::types::BoltLocalTime) {
    encode_struct_header(buf, tag::LOCAL_TIME, 1);
    encode_int(buf, t.nanoseconds);
}

fn encode_datetime(buf: &mut BytesMut, dt: &crate::types::BoltDateTime) {
    encode_struct_header(buf, tag::DATE_TIME, 3);
    encode_int(buf, dt.seconds);
    encode_int(buf, dt.nanoseconds);
    encode_int(buf, dt.tz_offset_seconds);
}

fn encode_datetime_zone_id(buf: &mut BytesMut, dt: &crate::types::BoltDateTimeZoneId) {
    encode_struct_header(buf, tag::DATE_TIME_ZONE_ID, 3);
    encode_int(buf, dt.seconds);
    encode_int(buf, dt.nanoseconds);
    encode_string(buf, &dt.tz_id);
}

fn encode_local_datetime(buf: &mut BytesMut, dt: &crate::types::BoltLocalDateTime) {
    encode_struct_header(buf, tag::LOCAL_DATE_TIME, 2);
    encode_int(buf, dt.seconds);
    encode_int(buf, dt.nanoseconds);
}

fn encode_duration(buf: &mut BytesMut, d: &crate::types::BoltDuration) {
    encode_struct_header(buf, tag::DURATION, 4);
    encode_int(buf, d.months);
    encode_int(buf, d.days);
    encode_int(buf, d.seconds);
    encode_int(buf, d.nanoseconds);
}

fn encode_point2d(buf: &mut BytesMut, p: &crate::types::BoltPoint2D) {
    encode_struct_header(buf, tag::POINT_2D, 3);
    encode_int(buf, p.srid);
    encode_float(buf, p.x);
    encode_float(buf, p.y);
}

fn encode_point3d(buf: &mut BytesMut, p: &crate::types::BoltPoint3D) {
    encode_struct_header(buf, tag::POINT_3D, 4);
    encode_int(buf, p.srid);
    encode_float(buf, p.x);
    encode_float(buf, p.y);
    encode_float(buf, p.z);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_null_marker() {
        let mut buf = BytesMut::new();
        encode_null(&mut buf);
        assert_eq!(&buf[..], &[0xC0]);
    }

    #[test]
    fn encode_booleans() {
        let mut buf = BytesMut::new();
        encode_bool(&mut buf, true);
        encode_bool(&mut buf, false);
        assert_eq!(&buf[..], &[0xC3, 0xC2]);
    }

    #[test]
    fn encode_tiny_int() {
        let mut buf = BytesMut::new();
        encode_int(&mut buf, 0);
        assert_eq!(&buf[..], &[0x00]);

        buf.clear();
        encode_int(&mut buf, 1);
        assert_eq!(&buf[..], &[0x01]);

        buf.clear();
        encode_int(&mut buf, 127);
        assert_eq!(&buf[..], &[0x7F]);

        buf.clear();
        encode_int(&mut buf, -1);
        assert_eq!(&buf[..], &[0xFF]);

        buf.clear();
        encode_int(&mut buf, -16);
        assert_eq!(&buf[..], &[0xF0]);
    }

    #[test]
    fn encode_int8() {
        let mut buf = BytesMut::new();
        encode_int(&mut buf, -17);
        assert_eq!(&buf[..], &[marker::INT_8, (-17i8) as u8]);

        buf.clear();
        encode_int(&mut buf, -128);
        assert_eq!(&buf[..], &[marker::INT_8, (-128i8) as u8]);
    }

    #[test]
    fn encode_int16() {
        let mut buf = BytesMut::new();
        encode_int(&mut buf, 128);
        assert_eq!(&buf[..], &[marker::INT_16, 0x00, 0x80]);

        buf.clear();
        encode_int(&mut buf, -129);
        let expected = (-129i16).to_be_bytes();
        assert_eq!(&buf[..], &[marker::INT_16, expected[0], expected[1]]);
    }

    #[test]
    fn encode_int32() {
        let mut buf = BytesMut::new();
        encode_int(&mut buf, 32768);
        let expected = 32768i32.to_be_bytes();
        assert_eq!(
            &buf[..],
            &[marker::INT_32, expected[0], expected[1], expected[2], expected[3]]
        );
    }

    #[test]
    fn encode_int64() {
        let mut buf = BytesMut::new();
        let val = i64::from(i32::MAX) + 1;
        encode_int(&mut buf, val);
        let expected = val.to_be_bytes();
        assert_eq!(buf[0], marker::INT_64);
        assert_eq!(&buf[1..], &expected);
    }

    #[test]
    fn encode_float64() {
        let mut buf = BytesMut::new();
        encode_float(&mut buf, 1.23);
        assert_eq!(buf[0], marker::FLOAT_64);
        let expected = 1.23f64.to_be_bytes();
        assert_eq!(&buf[1..], &expected);
    }

    #[test]
    fn encode_empty_string() {
        let mut buf = BytesMut::new();
        encode_string(&mut buf, "");
        assert_eq!(&buf[..], &[0x80]);
    }

    #[test]
    fn encode_tiny_string() {
        let mut buf = BytesMut::new();
        encode_string(&mut buf, "A");
        assert_eq!(&buf[..], &[0x81, 0x41]);
    }

    #[test]
    fn encode_string_16_bytes() {
        let s = "0123456789abcdef"; // 16 bytes, exceeds tiny
        let mut buf = BytesMut::new();
        encode_string(&mut buf, s);
        assert_eq!(buf[0], marker::STRING_8);
        assert_eq!(buf[1], 16);
        assert_eq!(&buf[2..], s.as_bytes());
    }

    #[test]
    fn encode_empty_list() {
        let mut buf = BytesMut::new();
        encode_list(&mut buf, &[]);
        assert_eq!(&buf[..], &[0x90]);
    }

    #[test]
    fn encode_tiny_list() {
        let mut buf = BytesMut::new();
        let items = vec![BoltValue::Integer(1), BoltValue::Integer(2), BoltValue::Integer(3)];
        encode_list(&mut buf, &items);
        assert_eq!(&buf[..], &[0x93, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn encode_empty_dict() {
        let mut buf = BytesMut::new();
        encode_dict(&mut buf, &std::collections::HashMap::new());
        assert_eq!(&buf[..], &[0xA0]);
    }

    #[test]
    fn encode_bytes_data() {
        let mut buf = BytesMut::new();
        encode_bytes(&mut buf, &[0xDE, 0xAD]);
        assert_eq!(&buf[..], &[marker::BYTES_8, 0x02, 0xDE, 0xAD]);
    }
}

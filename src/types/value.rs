//! Bolt value types.

use std::collections::HashMap;
use std::fmt;

/// Type alias for Bolt dictionaries (maps with string keys).
pub type BoltDict = HashMap<String, BoltValue>;

/// A value in the Bolt protocol, corresponding to PackStream types.
#[derive(Debug, Clone, PartialEq)]
pub enum BoltValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<BoltValue>),
    Dict(BoltDict),
    // Graph structures
    Node(BoltNode),
    Relationship(BoltRelationship),
    UnboundRelationship(BoltUnboundRelationship),
    Path(BoltPath),
    // Temporal
    Date(BoltDate),
    Time(BoltTime),
    LocalTime(BoltLocalTime),
    DateTime(BoltDateTime),
    DateTimeZoneId(BoltDateTimeZoneId),
    LocalDateTime(BoltLocalDateTime),
    Duration(BoltDuration),
    // Spatial
    Point2D(BoltPoint2D),
    Point3D(BoltPoint3D),
}

impl BoltValue {
    /// Returns the value as a string reference, if it is a `String` variant.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the value as an i64, if it is an `Integer` variant.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            _ => None,
        }
    }
}

// -- Graph structures --

#[derive(Debug, Clone, PartialEq)]
pub struct BoltNode {
    pub id: i64,
    pub labels: Vec<String>,
    pub properties: BoltDict,
    pub element_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltRelationship {
    pub id: i64,
    pub start_node_id: i64,
    pub end_node_id: i64,
    pub rel_type: String,
    pub properties: BoltDict,
    pub element_id: String,
    pub start_element_id: String,
    pub end_element_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltUnboundRelationship {
    pub id: i64,
    pub rel_type: String,
    pub properties: BoltDict,
    pub element_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltPath {
    pub nodes: Vec<BoltNode>,
    pub rels: Vec<BoltUnboundRelationship>,
    pub indices: Vec<i64>,
}

// -- Temporal structures --

#[derive(Debug, Clone, PartialEq)]
pub struct BoltDate {
    /// Days since Unix epoch.
    pub days: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltTime {
    /// Nanoseconds since midnight.
    pub nanoseconds: i64,
    /// Timezone offset in seconds.
    pub tz_offset_seconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltLocalTime {
    /// Nanoseconds since midnight.
    pub nanoseconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltDateTime {
    /// Seconds since Unix epoch.
    pub seconds: i64,
    /// Nanoseconds within the second.
    pub nanoseconds: i64,
    /// Timezone offset in seconds.
    pub tz_offset_seconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltDateTimeZoneId {
    /// Seconds since Unix epoch.
    pub seconds: i64,
    /// Nanoseconds within the second.
    pub nanoseconds: i64,
    /// IANA timezone identifier.
    pub tz_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltLocalDateTime {
    /// Seconds since Unix epoch.
    pub seconds: i64,
    /// Nanoseconds within the second.
    pub nanoseconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltDuration {
    pub months: i64,
    pub days: i64,
    pub seconds: i64,
    pub nanoseconds: i64,
}

// -- Spatial structures --

#[derive(Debug, Clone, PartialEq)]
pub struct BoltPoint2D {
    pub srid: i64,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoltPoint3D {
    pub srid: i64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

// -- Convenience conversions --

impl From<bool> for BoltValue {
    fn from(b: bool) -> Self {
        Self::Boolean(b)
    }
}

impl From<i64> for BoltValue {
    fn from(i: i64) -> Self {
        Self::Integer(i)
    }
}

impl From<i32> for BoltValue {
    fn from(i: i32) -> Self {
        Self::Integer(i64::from(i))
    }
}

impl From<f64> for BoltValue {
    fn from(f: f64) -> Self {
        Self::Float(f)
    }
}

impl From<String> for BoltValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for BoltValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_owned())
    }
}

impl From<Vec<u8>> for BoltValue {
    fn from(b: Vec<u8>) -> Self {
        Self::Bytes(b)
    }
}

impl From<Vec<BoltValue>> for BoltValue {
    fn from(v: Vec<BoltValue>) -> Self {
        Self::List(v)
    }
}

impl From<BoltDict> for BoltValue {
    fn from(d: BoltDict) -> Self {
        Self::Dict(d)
    }
}

impl From<BoltNode> for BoltValue {
    fn from(n: BoltNode) -> Self {
        Self::Node(n)
    }
}

impl From<BoltRelationship> for BoltValue {
    fn from(r: BoltRelationship) -> Self {
        Self::Relationship(r)
    }
}

impl fmt::Display for BoltValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Boolean(b) => write!(f, "{b}"),
            Self::Integer(i) => write!(f, "{i}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::String(s) => write!(f, "\"{s}\""),
            Self::Bytes(b) => write!(f, "<{} bytes>", b.len()),
            Self::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Self::Dict(dict) => {
                write!(f, "{{")?;
                for (i, (k, v)) in dict.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Self::Node(n) => write!(f, "(:{} {{id: {}}})", n.labels.join(":"), n.id),
            Self::Relationship(r) => write!(f, "-[:{}]-", r.rel_type),
            Self::UnboundRelationship(r) => write!(f, "-[:{}]-", r.rel_type),
            Self::Path(_) => write!(f, "<path>"),
            Self::Date(d) => write!(f, "date({})", d.days),
            Self::Time(t) => write!(f, "time({})", t.nanoseconds),
            Self::LocalTime(t) => write!(f, "localtime({})", t.nanoseconds),
            Self::DateTime(dt) => write!(f, "datetime({})", dt.seconds),
            Self::DateTimeZoneId(dt) => write!(f, "datetime({}, {})", dt.seconds, dt.tz_id),
            Self::LocalDateTime(dt) => write!(f, "localdatetime({})", dt.seconds),
            Self::Duration(d) => {
                write!(f, "duration({}m {}d {}s)", d.months, d.days, d.seconds)
            }
            Self::Point2D(p) => write!(f, "point({}, {}, {})", p.srid, p.x, p.y),
            Self::Point3D(p) => write!(f, "point({}, {}, {}, {})", p.srid, p.x, p.y, p.z),
        }
    }
}

//! Bolt protocol value types and graph structures.

mod value;

pub use value::{
    BoltDate, BoltDateTime, BoltDateTimeZoneId, BoltDict, BoltDuration, BoltLocalDateTime,
    BoltLocalTime, BoltNode, BoltPath, BoltPoint2D, BoltPoint3D, BoltRelationship, BoltTime,
    BoltUnboundRelationship, BoltValue,
};

/// PackStream structure tag bytes for graph and temporal types.
pub mod tag {
    pub const NODE: u8 = 0x4E;
    pub const RELATIONSHIP: u8 = 0x52;
    pub const UNBOUND_RELATIONSHIP: u8 = 0x72;
    pub const PATH: u8 = 0x50;
    pub const DATE: u8 = 0x44;
    pub const TIME: u8 = 0x54;
    pub const LOCAL_TIME: u8 = 0x74;
    pub const DATE_TIME: u8 = 0x49;
    pub const DATE_TIME_ZONE_ID: u8 = 0x69;
    pub const LOCAL_DATE_TIME: u8 = 0x64;
    pub const DURATION: u8 = 0x45;
    pub const POINT_2D: u8 = 0x58;
    pub const POINT_3D: u8 = 0x59;
}

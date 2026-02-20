//! Bolt protocol messages.

pub mod decode;
pub mod encode;
pub mod request;
pub mod response;

pub use request::ClientMessage;
pub use response::ServerMessage;

/// Message signature bytes.
pub mod sig {
    // Client → Server
    pub const HELLO: u8 = 0x01;
    pub const LOGON: u8 = 0x6A;
    pub const LOGOFF: u8 = 0x6B;
    pub const GOODBYE: u8 = 0x02;
    pub const RESET: u8 = 0x0F;
    pub const RUN: u8 = 0x10;
    pub const PULL: u8 = 0x3F;
    pub const DISCARD: u8 = 0x2F;
    pub const BEGIN: u8 = 0x11;
    pub const COMMIT: u8 = 0x12;
    pub const ROLLBACK: u8 = 0x13;

    // Server → Client
    pub const SUCCESS: u8 = 0x70;
    pub const RECORD: u8 = 0x71;
    pub const FAILURE: u8 = 0x7F;
    pub const IGNORED: u8 = 0x7E;
}

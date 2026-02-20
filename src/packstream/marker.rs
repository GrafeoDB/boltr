//! PackStream marker byte constants.

// Null
pub const NULL: u8 = 0xC0;

// Boolean
pub const FALSE: u8 = 0xC2;
pub const TRUE: u8 = 0xC3;

// Float (IEEE 754 double-precision)
pub const FLOAT_64: u8 = 0xC1;

// Integer (beyond TINY_INT range)
pub const INT_8: u8 = 0xC8;
pub const INT_16: u8 = 0xC9;
pub const INT_32: u8 = 0xCA;
pub const INT_64: u8 = 0xCB;

// TINY_INT: single byte, range -16..=127
// Positive: 0x00..=0x7F (0..127)
// Negative: 0xF0..=0xFF (-16..-1)

// Bytes
pub const BYTES_8: u8 = 0xCC;
pub const BYTES_16: u8 = 0xCD;
pub const BYTES_32: u8 = 0xCE;

// String
// TINY_STRING: 0x80..=0x8F (high nibble 0x8, low = byte length 0..15)
pub const STRING_8: u8 = 0xD0;
pub const STRING_16: u8 = 0xD1;
pub const STRING_32: u8 = 0xD2;

// List
// TINY_LIST: 0x90..=0x9F (high nibble 0x9, low = item count 0..15)
pub const LIST_8: u8 = 0xD4;
pub const LIST_16: u8 = 0xD5;
pub const LIST_32: u8 = 0xD6;

// Dictionary (Map)
// TINY_DICT: 0xA0..=0xAF (high nibble 0xA, low = entry count 0..15)
pub const DICT_8: u8 = 0xD8;
pub const DICT_16: u8 = 0xD9;
pub const DICT_32: u8 = 0xDA;

// Structure
// TINY_STRUCT: 0xB0..=0xBF (high nibble 0xB, low = field count 0..15)

// High-nibble masks for tiny types.
pub const TINY_STRING_NIBBLE: u8 = 0x80;
pub const TINY_LIST_NIBBLE: u8 = 0x90;
pub const TINY_DICT_NIBBLE: u8 = 0xA0;
pub const TINY_STRUCT_NIBBLE: u8 = 0xB0;

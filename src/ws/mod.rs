//! WebSocket transport for Bolt connections.
//!
//! This module provides WebSocket support for both server and client
//! components, allowing Bolt protocol messages to be exchanged over
//! `ws://` and `wss://` connections.
//!
//! Enable with the `ws` feature flag in `Cargo.toml`.

mod stream;

pub mod server;

pub use stream::WsStream;

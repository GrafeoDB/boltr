//! BoltR: A pure-Rust Bolt v5.x wire protocol library.
//!
//! This crate implements the Bolt binary protocol used by Neo4j and compatible
//! graph databases. It provides both server and client components for building
//! Bolt-compatible applications.
//!
//! # Architecture
//!
//! - **`packstream`**, binary encoding/decoding (PackStream format)
//! - **`chunk`**, message framing (2-byte length-prefixed chunks)
//! - **`message`**, protocol message types and serialization
//! - **`types`**, Bolt value types (scalars, graph structures, temporal, spatial)
//! - **`server`**, server framework with `BoltBackend` trait
//! - **`client`**, client for connecting to Bolt servers (feature-gated)

pub mod chunk;
pub mod error;
pub mod message;
pub mod packstream;
pub mod server;
pub mod types;
pub mod version;

#[cfg(feature = "client")]
pub mod client;

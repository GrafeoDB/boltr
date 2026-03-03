//! BoltR: A pure-Rust Bolt v5.x wire protocol library.
//!
//! This crate implements the Bolt binary protocol used by Neo4j and compatible
//! graph databases. It provides both server and client components for building
//! Bolt-compatible applications.
//!
//! # Quick start (server)
//!
//! ```rust,no_run
//! use std::net::SocketAddr;
//! use boltr::server::{BoltServer, BoltBackend};
//!
//! # async fn example(backend: impl BoltBackend) -> Result<(), boltr::error::BoltError> {
//! let addr: SocketAddr = "127.0.0.1:7687".parse().unwrap();
//! BoltServer::builder(backend)
//!     .max_sessions(100)
//!     .serve(addr)
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Quick start (client)
//!
//! ```rust,no_run
//! # #[cfg(feature = "client")]
//! # async fn example() -> Result<(), boltr::error::BoltError> {
//! use boltr::client::BoltSession;
//!
//! let addr = "127.0.0.1:7687".parse().unwrap();
//! let mut session = BoltSession::connect(addr).await?;
//! let result = session.run("RETURN 1 AS n").await?;
//!
//! for record in &result.records {
//!     println!("{:?}", record);
//! }
//!
//! session.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Architecture
//!
//! - **`packstream`**, binary encoding/decoding (PackStream format)
//! - **`chunk`**, message framing (2-byte length-prefixed chunks)
//! - **`message`**, protocol message types and serialization
//! - **`types`**, Bolt value types (scalars, graph structures, temporal, spatial)
//! - **`server`**, server framework with `BoltBackend` trait
//! - **`client`**, client for connecting to Bolt servers (feature-gated)

#![forbid(unsafe_code)]

pub mod chunk;
pub mod error;
pub mod message;
pub mod packstream;
pub mod server;
pub mod types;
pub mod version;

#[cfg(feature = "client")]
pub mod client;

//! Bolt client — connects to a Bolt server and runs queries.
//!
//! Feature-gated behind `client`. Primarily intended for integration testing.

mod connection;
mod session;

pub use connection::BoltConnection;
pub use session::BoltSession;

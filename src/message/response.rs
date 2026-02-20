//! Server-to-client Bolt messages.

use crate::types::{BoltDict, BoltValue};

/// A message sent from the server to the client.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    /// Request completed successfully. Metadata varies by context.
    Success { metadata: BoltDict },

    /// A row of query results.
    Record { data: Vec<BoltValue> },

    /// Request failed. Contains error code and message.
    Failure { metadata: BoltDict },

    /// Request was ignored (connection is in an error state).
    Ignored,
}

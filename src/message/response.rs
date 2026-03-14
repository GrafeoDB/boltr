//! Server-to-client Bolt messages.

use std::fmt;

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

impl fmt::Display for ServerMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success { .. } => write!(f, "SUCCESS"),
            Self::Record { data } => write!(f, "RECORD({} fields)", data.len()),
            Self::Failure { metadata } => {
                let code = metadata
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                write!(f, "FAILURE({code})")
            }
            Self::Ignored => write!(f, "IGNORED"),
        }
    }
}

//! Client-to-server Bolt messages.

use crate::types::{BoltDict, BoltValue};

/// A message sent from the client to the server.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    /// Initialize connection. Sent once after handshake.
    Hello { extra: BoltDict },

    /// Authenticate after HELLO (Bolt 5.1+).
    Logon { auth: BoltDict },

    /// De-authenticate (Bolt 5.1+).
    Logoff,

    /// Gracefully close the connection.
    Goodbye,

    /// Reset the connection to a clean state, aborting any pending work.
    Reset,

    /// Execute a query (auto-commit or within a transaction).
    Run {
        query: String,
        parameters: BoltDict,
        extra: BoltDict,
    },

    /// Pull results from the last RUN.
    Pull { extra: BoltDict },

    /// Discard results from the last RUN.
    Discard { extra: BoltDict },

    /// Begin an explicit transaction.
    Begin { extra: BoltDict },

    /// Commit the current explicit transaction.
    Commit,

    /// Roll back the current explicit transaction.
    Rollback,
}

impl ClientMessage {
    /// Creates a PULL message requesting all remaining records.
    pub fn pull_all() -> Self {
        Self::Pull {
            extra: BoltDict::from([("n".to_string(), BoltValue::Integer(-1))]),
        }
    }

    /// Creates a PULL message requesting `n` records.
    pub fn pull_n(n: i64) -> Self {
        Self::Pull {
            extra: BoltDict::from([("n".to_string(), BoltValue::Integer(n))]),
        }
    }

    /// Creates a DISCARD message discarding all remaining records.
    pub fn discard_all() -> Self {
        Self::Discard {
            extra: BoltDict::from([("n".to_string(), BoltValue::Integer(-1))]),
        }
    }
}

//! Client-to-server Bolt messages.

use std::fmt;

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

    /// Request routing table for cluster-aware drivers (Bolt 5.2+).
    Route {
        routing: BoltDict,
        bookmarks: Vec<String>,
        extra: BoltDict,
    },

    /// Client telemetry data (Bolt 5.4+). Server may safely ignore.
    Telemetry { api: i64 },
}

impl fmt::Display for ClientMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hello { .. } => write!(f, "HELLO"),
            Self::Logon { .. } => write!(f, "LOGON"),
            Self::Logoff => write!(f, "LOGOFF"),
            Self::Goodbye => write!(f, "GOODBYE"),
            Self::Reset => write!(f, "RESET"),
            Self::Run { query, .. } => write!(f, "RUN {query:?}"),
            Self::Pull { .. } => write!(f, "PULL"),
            Self::Discard { .. } => write!(f, "DISCARD"),
            Self::Begin { .. } => write!(f, "BEGIN"),
            Self::Commit => write!(f, "COMMIT"),
            Self::Rollback => write!(f, "ROLLBACK"),
            Self::Route { .. } => write!(f, "ROUTE"),
            Self::Telemetry { api } => write!(f, "TELEMETRY({api})"),
        }
    }
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

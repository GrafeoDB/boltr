//! Error types for the Bolt protocol.

use std::collections::HashMap;

use crate::types::BoltValue;

/// Errors that can occur during Bolt protocol operations.
#[derive(Debug, thiserror::Error)]
pub enum BoltError {
    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("authentication error: {0}")]
    Authentication(String),

    #[error("session error: {0}")]
    Session(String),

    #[error("transaction error: {0}")]
    Transaction(String),

    #[error("query error {code}: {message}")]
    Query { code: String, message: String },

    #[error("resource exhausted: {0}")]
    ResourceExhausted(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("backend error: {0}")]
    Backend(String),
}

impl BoltError {
    /// Wraps any displayable error as a backend error.
    pub fn backend(e: impl std::fmt::Display) -> Self {
        Self::Backend(e.to_string())
    }

    /// Converts this error into a Bolt FAILURE metadata dictionary.
    pub fn to_failure_metadata(&self) -> HashMap<String, BoltValue> {
        let (code, message) = match self {
            Self::Protocol(m) => ("Neo.ClientError.Request.Invalid", m.clone()),
            Self::Authentication(m) => ("Neo.ClientError.Security.Unauthorized", m.clone()),
            Self::Session(m) => ("Neo.ClientError.Request.Invalid", m.clone()),
            Self::Transaction(m) => {
                ("Neo.ClientError.Transaction.TransactionStartFailed", m.clone())
            }
            Self::Query { code, message } => (code.as_str(), message.clone()),
            Self::ResourceExhausted(m) => {
                ("Neo.TransientError.General.MemoryPoolOutOfMemoryError", m.clone())
            }
            Self::Io(e) => (
                "Neo.TransientError.General.DatabaseUnavailable",
                e.to_string(),
            ),
            Self::Backend(m) => ("Neo.DatabaseError.General.UnknownError", m.clone()),
        };
        HashMap::from([
            ("code".to_string(), BoltValue::String(code.to_string())),
            ("message".to_string(), BoltValue::String(message)),
        ])
    }
}

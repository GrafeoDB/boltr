//! Bolt connection state machine.

use crate::message::ClientMessage;

/// The state of a Bolt connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Handshake complete, waiting for HELLO.
    Negotiation,
    /// HELLO received, waiting for LOGON.
    Authentication,
    /// Authenticated and idle, ready for RUN or BEGIN.
    Ready,
    /// Auto-commit query running, expecting PULL or DISCARD.
    Streaming,
    /// Inside explicit transaction, idle.
    TxReady,
    /// Inside explicit transaction, query running.
    TxStreaming,
    /// An error occurred; only RESET or GOODBYE accepted.
    Failed,
    /// Terminal state — connection should be closed.
    Defunct,
}

impl ConnectionState {
    /// Returns whether a given client message is valid in this state.
    pub fn accepts(&self, msg: &ClientMessage) -> bool {
        match self {
            Self::Negotiation => matches!(msg, ClientMessage::Hello { .. }),
            Self::Authentication => matches!(
                msg,
                ClientMessage::Logon { .. } | ClientMessage::Goodbye
            ),
            Self::Ready => matches!(
                msg,
                ClientMessage::Run { .. }
                    | ClientMessage::Begin { .. }
                    | ClientMessage::Reset
                    | ClientMessage::Goodbye
                    | ClientMessage::Logoff
            ),
            Self::Streaming => matches!(
                msg,
                ClientMessage::Pull { .. }
                    | ClientMessage::Discard { .. }
                    | ClientMessage::Reset
                    | ClientMessage::Goodbye
            ),
            Self::TxReady => matches!(
                msg,
                ClientMessage::Run { .. }
                    | ClientMessage::Commit
                    | ClientMessage::Rollback
                    | ClientMessage::Reset
                    | ClientMessage::Goodbye
            ),
            Self::TxStreaming => matches!(
                msg,
                ClientMessage::Pull { .. }
                    | ClientMessage::Discard { .. }
                    | ClientMessage::Reset
                    | ClientMessage::Goodbye
            ),
            Self::Failed => matches!(
                msg,
                ClientMessage::Reset | ClientMessage::Goodbye
            ),
            Self::Defunct => false,
        }
    }

    /// Compute the next state after successfully processing a message.
    pub fn transition_success(&self, msg: &ClientMessage) -> Self {
        match (self, msg) {
            // Handshake flow
            (Self::Negotiation, ClientMessage::Hello { .. }) => Self::Authentication,
            (Self::Authentication, ClientMessage::Logon { .. }) => Self::Ready,

            // Auto-commit query
            (Self::Ready, ClientMessage::Run { .. }) => Self::Streaming,
            (Self::Streaming, ClientMessage::Pull { .. }) => Self::Streaming, // has_more check done externally
            (Self::Streaming, ClientMessage::Discard { .. }) => Self::Streaming,

            // Explicit transaction
            (Self::Ready, ClientMessage::Begin { .. }) => Self::TxReady,
            (Self::TxReady, ClientMessage::Run { .. }) => Self::TxStreaming,
            (Self::TxStreaming, ClientMessage::Pull { .. }) => Self::TxStreaming,
            (Self::TxStreaming, ClientMessage::Discard { .. }) => Self::TxStreaming,
            (Self::TxReady, ClientMessage::Commit) => Self::Ready,
            (Self::TxReady, ClientMessage::Rollback) => Self::Ready,

            // Reset (from any non-defunct state)
            (_, ClientMessage::Reset) => Self::Ready,

            // Logoff
            (Self::Ready, ClientMessage::Logoff) => Self::Authentication,

            // Goodbye
            (_, ClientMessage::Goodbye) => Self::Defunct,

            // Anything else stays the same (should not happen if accepts() is checked)
            _ => *self,
        }
    }

    /// Compute the next state after a message fails.
    pub fn transition_failure(&self, msg: &ClientMessage) -> Self {
        match msg {
            ClientMessage::Goodbye => Self::Defunct,
            ClientMessage::Reset => Self::Defunct, // RESET failure is fatal
            _ => Self::Failed,
        }
    }

    /// Returns true when streaming is complete (no more records).
    /// Used by the connection handler to transition Streaming→Ready.
    pub fn complete_streaming(&self) -> Self {
        match self {
            Self::Streaming => Self::Ready,
            Self::TxStreaming => Self::TxReady,
            other => *other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BoltDict;

    fn hello() -> ClientMessage {
        ClientMessage::Hello { extra: BoltDict::new() }
    }
    fn logon() -> ClientMessage {
        ClientMessage::Logon { auth: BoltDict::new() }
    }
    fn run() -> ClientMessage {
        ClientMessage::Run {
            query: "RETURN 1".into(),
            parameters: BoltDict::new(),
            extra: BoltDict::new(),
        }
    }
    fn pull() -> ClientMessage {
        ClientMessage::pull_all()
    }
    fn begin() -> ClientMessage {
        ClientMessage::Begin { extra: BoltDict::new() }
    }

    #[test]
    fn negotiation_accepts_only_hello() {
        assert!(ConnectionState::Negotiation.accepts(&hello()));
        assert!(!ConnectionState::Negotiation.accepts(&run()));
        assert!(!ConnectionState::Negotiation.accepts(&ClientMessage::Goodbye));
    }

    #[test]
    fn authentication_accepts_logon_and_goodbye() {
        assert!(ConnectionState::Authentication.accepts(&logon()));
        assert!(ConnectionState::Authentication.accepts(&ClientMessage::Goodbye));
        assert!(!ConnectionState::Authentication.accepts(&run()));
    }

    #[test]
    fn ready_state_transitions() {
        let s = ConnectionState::Ready;
        assert!(s.accepts(&run()));
        assert!(s.accepts(&begin()));
        assert!(s.accepts(&ClientMessage::Reset));
        assert!(s.accepts(&ClientMessage::Goodbye));
        assert!(!s.accepts(&pull()));
        assert!(!s.accepts(&ClientMessage::Commit));
    }

    #[test]
    fn streaming_to_ready() {
        let s = ConnectionState::Streaming;
        assert!(s.accepts(&pull()));
        assert!(s.accepts(&ClientMessage::Discard { extra: BoltDict::new() }));
        assert!(!s.accepts(&run()));
        assert_eq!(s.complete_streaming(), ConnectionState::Ready);
    }

    #[test]
    fn tx_flow() {
        let s = ConnectionState::Ready;
        let s = s.transition_success(&begin());
        assert_eq!(s, ConnectionState::TxReady);

        let s = s.transition_success(&run());
        assert_eq!(s, ConnectionState::TxStreaming);

        let s = s.complete_streaming();
        assert_eq!(s, ConnectionState::TxReady);

        let s = s.transition_success(&ClientMessage::Commit);
        assert_eq!(s, ConnectionState::Ready);
    }

    #[test]
    fn failed_state() {
        let s = ConnectionState::Failed;
        assert!(s.accepts(&ClientMessage::Reset));
        assert!(s.accepts(&ClientMessage::Goodbye));
        assert!(!s.accepts(&run()));
        assert!(!s.accepts(&pull()));
    }

    #[test]
    fn failure_transitions_to_failed() {
        let s = ConnectionState::Ready;
        assert_eq!(s.transition_failure(&run()), ConnectionState::Failed);
    }

    #[test]
    fn reset_from_failed() {
        let s = ConnectionState::Failed;
        assert_eq!(s.transition_success(&ClientMessage::Reset), ConnectionState::Ready);
    }
}

//! Bolt connection state machine.

use std::fmt;

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
    /// Terminal state, connection should be closed.
    Defunct,
}

impl fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Negotiation => write!(f, "Negotiation"),
            Self::Authentication => write!(f, "Authentication"),
            Self::Ready => write!(f, "Ready"),
            Self::Streaming => write!(f, "Streaming"),
            Self::TxReady => write!(f, "TxReady"),
            Self::TxStreaming => write!(f, "TxStreaming"),
            Self::Failed => write!(f, "Failed"),
            Self::Defunct => write!(f, "Defunct"),
        }
    }
}

impl ConnectionState {
    /// Returns whether a given client message is valid in this state.
    #[must_use]
    pub fn accepts(&self, msg: &ClientMessage) -> bool {
        match self {
            Self::Negotiation => matches!(msg, ClientMessage::Hello { .. }),
            Self::Authentication => {
                matches!(msg, ClientMessage::Logon { .. } | ClientMessage::Goodbye)
            }
            Self::Ready => matches!(
                msg,
                ClientMessage::Run { .. }
                    | ClientMessage::Begin { .. }
                    | ClientMessage::Route { .. }
                    | ClientMessage::Telemetry { .. }
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
            // Several results can be open in a transaction (Bolt 4.0+ qid),
            // so RUN is accepted while streaming, and ROLLBACK ends the
            // transaction whatever is still open.
            Self::TxStreaming => matches!(
                msg,
                ClientMessage::Pull { .. }
                    | ClientMessage::Discard { .. }
                    | ClientMessage::Run { .. }
                    | ClientMessage::Rollback
                    | ClientMessage::Reset
                    | ClientMessage::Goodbye
            ),
            Self::Failed => matches!(msg, ClientMessage::Reset | ClientMessage::Goodbye),
            Self::Defunct => false,
        }
    }

    /// Compute the next state after successfully processing a message.
    #[must_use]
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
            (Self::TxReady | Self::TxStreaming, ClientMessage::Run { .. }) => Self::TxStreaming,
            (Self::TxStreaming, ClientMessage::Pull { .. }) => Self::TxStreaming,
            (Self::TxStreaming, ClientMessage::Discard { .. }) => Self::TxStreaming,
            (Self::TxReady, ClientMessage::Commit) => Self::Ready,
            (Self::TxReady | Self::TxStreaming, ClientMessage::Rollback) => Self::Ready,

            // Reset (from any authenticated state). Before LOGON there is
            // nothing to reset to: RESET is not accepted there.
            (_, ClientMessage::Reset) if self.is_authenticated() => Self::Ready,

            // Logoff
            (Self::Ready, ClientMessage::Logoff) => Self::Authentication,

            // Goodbye
            (_, ClientMessage::Goodbye) => Self::Defunct,

            // Anything else stays the same (should not happen if accepts() is checked)
            _ => *self,
        }
    }

    /// Returns true once the connection has completed LOGON, that is in
    /// every state except `Negotiation`, `Authentication` and `Defunct`.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        !matches!(
            self,
            Self::Negotiation | Self::Authentication | Self::Defunct
        )
    }

    /// Compute the next state after a message fails.
    ///
    /// Any failure before authentication completes (a failed HELLO or LOGON,
    /// or a malformed message) is fatal: the connection goes to `Defunct`, as
    /// the Bolt specification requires. Recovering through `Failed` would let
    /// a subsequent RESET move an unauthenticated connection to `Ready`.
    #[must_use]
    pub fn transition_failure(&self, msg: &ClientMessage) -> Self {
        if !self.is_authenticated() {
            return Self::Defunct;
        }
        match msg {
            ClientMessage::Goodbye => Self::Defunct,
            ClientMessage::Reset => Self::Defunct, // RESET failure is fatal
            _ => Self::Failed,
        }
    }

    /// The state to enter after a protocol violation that is not tied to a
    /// specific decoded message (for example an undecodable message, or a
    /// message that is not valid in the current state).
    #[must_use]
    pub fn after_protocol_violation(&self) -> Self {
        if self.is_authenticated() {
            Self::Failed
        } else {
            Self::Defunct
        }
    }

    /// Returns the state after streaming completes (no more records).
    /// Used by the connection handler to transition Streaming to Ready.
    #[must_use]
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
        ClientMessage::Hello {
            extra: BoltDict::new(),
        }
    }
    fn logon() -> ClientMessage {
        ClientMessage::Logon {
            auth: BoltDict::new(),
        }
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
        ClientMessage::Begin {
            extra: BoltDict::new(),
        }
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
        assert!(s.accepts(&ClientMessage::Discard {
            extra: BoltDict::new()
        }));
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
        assert_eq!(
            s.transition_success(&ClientMessage::Reset),
            ConnectionState::Ready
        );
    }

    const ALL_STATES: [ConnectionState; 8] = [
        ConnectionState::Negotiation,
        ConnectionState::Authentication,
        ConnectionState::Ready,
        ConnectionState::Streaming,
        ConnectionState::TxReady,
        ConnectionState::TxStreaming,
        ConnectionState::Failed,
        ConnectionState::Defunct,
    ];

    fn all_messages() -> Vec<ClientMessage> {
        vec![
            hello(),
            logon(),
            ClientMessage::Logoff,
            ClientMessage::Goodbye,
            ClientMessage::Reset,
            run(),
            pull(),
            ClientMessage::discard_all(),
            begin(),
            ClientMessage::Commit,
            ClientMessage::Rollback,
            ClientMessage::Route {
                routing: BoltDict::new(),
                bookmarks: vec![],
                extra: BoltDict::new(),
            },
            ClientMessage::Telemetry { api: 0 },
        ]
    }

    /// Regression: a failed LOGON (or HELLO) used to move the connection to
    /// `Failed`, from which RESET leads to `Ready` without authentication.
    #[test]
    fn failures_before_authentication_are_fatal() {
        for state in [
            ConnectionState::Negotiation,
            ConnectionState::Authentication,
        ] {
            for msg in all_messages() {
                assert_eq!(
                    state.transition_failure(&msg),
                    ConnectionState::Defunct,
                    "{state} + {msg}"
                );
            }
            assert_eq!(state.after_protocol_violation(), ConnectionState::Defunct);
        }
    }

    #[test]
    fn failures_after_authentication_go_to_failed() {
        for state in [
            ConnectionState::Ready,
            ConnectionState::Streaming,
            ConnectionState::TxReady,
            ConnectionState::TxStreaming,
            ConnectionState::Failed,
        ] {
            assert_eq!(state.transition_failure(&run()), ConnectionState::Failed);
            assert_eq!(state.after_protocol_violation(), ConnectionState::Failed);
            assert_eq!(
                state.transition_failure(&ClientMessage::Reset),
                ConnectionState::Defunct
            );
        }
    }

    /// No sequence of successful or failed transitions may reach an
    /// authenticated state from a pre-authentication state except a
    /// successful LOGON.
    #[test]
    fn only_logon_authenticates() {
        for state in [
            ConnectionState::Negotiation,
            ConnectionState::Authentication,
        ] {
            for msg in all_messages() {
                let ok = state.transition_success(&msg);
                let failed = state.transition_failure(&msg);
                assert!(!failed.is_authenticated(), "{state} + {msg} failure");
                if ok.is_authenticated() {
                    assert!(
                        state == ConnectionState::Authentication
                            && matches!(msg, ClientMessage::Logon { .. })
                            && state.accepts(&msg),
                        "{state} + {msg} authenticated without LOGON"
                    );
                }
            }
        }
    }

    #[test]
    fn reset_is_accepted_in_every_authenticated_state() {
        for state in ALL_STATES {
            assert_eq!(
                state.accepts(&ClientMessage::Reset),
                state.is_authenticated(),
                "{state}"
            );
        }
    }

    #[test]
    fn goodbye_is_accepted_everywhere_but_negotiation_and_defunct() {
        for state in ALL_STATES {
            let expected = !matches!(
                state,
                ConnectionState::Negotiation | ConnectionState::Defunct
            );
            assert_eq!(state.accepts(&ClientMessage::Goodbye), expected, "{state}");
        }
    }

    #[test]
    fn defunct_accepts_nothing() {
        for msg in all_messages() {
            assert!(!ConnectionState::Defunct.accepts(&msg), "{msg}");
        }
    }

    #[test]
    fn failed_ignores_everything_but_reset_and_goodbye() {
        for msg in all_messages() {
            let expected = matches!(msg, ClientMessage::Reset | ClientMessage::Goodbye);
            assert_eq!(ConnectionState::Failed.accepts(&msg), expected, "{msg}");
        }
    }

    #[test]
    fn logoff_returns_to_authentication() {
        let s = ConnectionState::Ready;
        assert!(s.accepts(&ClientMessage::Logoff));
        let s = s.transition_success(&ClientMessage::Logoff);
        assert_eq!(s, ConnectionState::Authentication);
        assert!(!s.is_authenticated());
        assert!(!s.accepts(&run()));
        assert!(s.accepts(&logon()));
    }

    /// Bolt 4.0+: several results may be open in one transaction, so RUN is
    /// valid while streaming; ROLLBACK ends the transaction regardless, but
    /// COMMIT waits until every result is consumed.
    #[test]
    fn tx_streaming_accepts_more_runs_and_rollback() {
        let s = ConnectionState::TxStreaming;
        assert!(s.accepts(&run()));
        assert_eq!(s.transition_success(&run()), ConnectionState::TxStreaming);
        assert!(s.accepts(&ClientMessage::Rollback));
        assert_eq!(
            s.transition_success(&ClientMessage::Rollback),
            ConnectionState::Ready
        );
        assert!(!s.accepts(&ClientMessage::Commit));
        assert!(!s.accepts(&begin()));

        // Auto-commit streaming still allows a single result only.
        assert!(!ConnectionState::Streaming.accepts(&run()));
        assert!(!ConnectionState::Streaming.accepts(&ClientMessage::Rollback));
    }

    #[test]
    fn route_and_telemetry_only_in_ready() {
        let route = ClientMessage::Route {
            routing: BoltDict::new(),
            bookmarks: vec![],
            extra: BoltDict::new(),
        };
        let telemetry = ClientMessage::Telemetry { api: 1 };
        for state in ALL_STATES {
            let expected = state == ConnectionState::Ready;
            assert_eq!(state.accepts(&route), expected, "{state}");
            assert_eq!(state.accepts(&telemetry), expected, "{state}");
        }
    }
}

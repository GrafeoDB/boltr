# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - Unreleased

### Security
- **Authentication bypass**: a failed LOGON (or a failed HELLO, or an undecodable message before LOGON) moved the connection to `Failed`, from which RESET reached `Ready` without authenticating, so a client with wrong credentials could run queries. Any failure before authentication now closes the connection (`Defunct`), as the Bolt specification requires, and the connection also tracks on its own whether LOGON succeeded before dispatching any other message.
- **Process abort on deeply nested values**: PackStream decoding recursed without a limit, so one message of nested one-element lists overflowed the stack and aborted the whole server. Nesting is now limited to `packstream::decode::MAX_NESTING_DEPTH` (128) levels.
- **Memory exhaustion before authentication**: a decoded value takes about 170 bytes per one-byte PackStream value, so a single 16 MiB HELLO could make the server allocate gigabytes before the client authenticated (a 1 MiB HELLO measured 254 MiB of peak heap). Messages before LOGON (and after LOGOFF) are now limited to `max_unauthenticated_message_size` (64 KiB by default, a few tens of MB decoded at worst); the general `max_message_size` applies once authenticated.

### Fixed
- **Session leak on disconnect**: when a write failed (client gone mid-response), the connection returned before its cleanup, so the session stayed registered in the `SessionManager` and was never closed in the backend. With `max_sessions`, a few such disconnects locked every client out. Cleanup (roll back an open transaction, close the backend session) now runs however the connection ends.
- **Backend session leak** when `max_sessions` rejected a HELLO: the session created by the backend was never closed.
- **DISCARD** ignored `n` (it always discarded everything) and dropped the result summary, so drivers lost `stats` and the bookmark when consuming a partly read result. It now honours `n`, and its final SUCCESS carries the summary, like PULL.
- PULL and DISCARD reject `n = 0` and non-integer `n`; only `-1` or a positive count is valid.
- Messages that are not valid in the current state are answered with FAILURE (`Neo.ClientError.Request.Invalid`) instead of IGNORED. IGNORED is only sent in the `Failed` state, as specified.
- Only RUN refreshed a session's idle timer, so a client paging through a result was reaped while active. Every message now counts as activity.
- `idle_timeout(Duration::ZERO)` panicked the idle reaper.
- PackStream structure field counts were not validated, so a structure declaring too few fields silently consumed the following values of the message.
- Declared list and dictionary sizes were reserved up front (bounded only by the message size times the size of a `BoltValue`); pre-allocation is now capped and containers grow as elements are decoded.
- Decode errors embedded the offending value, so a malformed multi-megabyte value produced a multi-megabyte error message (echoed back in FAILURE). They now name the value type only.
- Message decoding accepted any first byte as a structure marker.
- The server closed connections while client input was still unread, which makes the operating system send a reset that can discard the final FAILURE before the client reads it (drivers then report a retryable connection error instead of, for example, an authentication error). The server now shuts down its write side and drains input for up to a second before closing.
- An oversized message is now answered with a FAILURE before the connection closes.
- `WsStream` treated an empty binary WebSocket frame as the end of the stream.
- The accept loop spun at full CPU on persistent `accept()` errors (for example when out of file descriptors); it now backs off for 100 ms.
- Metadata `hints` returned by `BoltBackend::get_server_info` are no longer overwritten in the HELLO response.
- Client: `BoltConnection::send` did not flush, so over WebSocket nothing was sent and `BoltSession::connect_ws` hung.
- Client: NOOP keep-alive chunks, which Neo4j sends during long-running queries, were decoded as messages and failed.
- Client: the handshake accepted any server answer as the negotiated version; it now requires one of the proposed versions, so connecting to a non-Bolt port fails immediately with a clear error.
- Clippy `chunks_exact_to_as_chunks` lint (Rust 1.98) in version negotiation.

### Added
- `BoltServer::handshake_timeout(Duration)`: a new connection must complete the TLS and WebSocket handshakes (when enabled), the Bolt handshake, HELLO and LOGON within this time or it is closed. Defaults to `DEFAULT_HANDSHAKE_TIMEOUT` (30 s); `Duration::ZERO` disables it.
- `BoltServer::serve_listener(TcpListener)` and `BoltServer::ws_serve_listener(TcpListener)` to serve an already bound listener (for example one bound to port 0 in tests).
- `BoltServer::max_unauthenticated_message_size(bytes)` and `DEFAULT_MAX_UNAUTHENTICATED_MESSAGE_SIZE` (64 KiB): the message size limit before authentication, capped by `max_message_size`.
- Several open results per explicit transaction (Bolt 4.0+ `qid`): RUN inside a transaction answers with a `qid` in its SUCCESS, a new RUN is accepted while earlier results are still open, and PULL and DISCARD select a result with `{"qid": n}` (`-1` or no `qid` means the most recent RUN). Drivers rely on this once a result in a transaction exceeds their fetch size. No backend change is needed: results are buffered by the server framework (at most 1024 open per transaction).
- `boltr::client::Counters`, with `QueryResult::counters()` and `Counters::from_summary()`: typed write counters read from the summary `stats` (Neo4j key names, missing keys read as zero) plus `contains_updates()` and `contains_system_updates()`.
- `boltr::client::QueryResult` is now exported (it was returned by `BoltSession::run` but could not be named).
- `ConnectionState::is_authenticated()` and `ConnectionState::after_protocol_violation()`.
- `version::proposals_cover()` and `packstream::decode::MAX_NESTING_DEPTH`.
- Tests: PackStream property tests (proptest) over every value type and message, exact wire bytes at every integer and size boundary, chunk framing at the 65535 byte limit, state machine transitions for every state, and end-to-end tests of full sessions over TCP, WebSocket and TLS (certificate generated with rcgen), including interleaved results by `qid`, malformed input, disconnects, timeouts and the regressions above. A counting allocator checks the memory an unauthenticated client can cause.
- A cargo-fuzz harness in `fuzz/` (PackStream values, messages, server connection). It is not run in CI.

### Changed
- `ConnectionState::transition_failure` returns `Defunct` for any failure before authentication, and `transition_success` no longer maps RESET to `Ready` from `Negotiation` or `Authentication`.
- Connections that do not authenticate within the handshake timeout (30 s by default) are closed. This also applies to `ws::server::accept_ws` and `ws::server::handle_ws`, as does the 64 KiB message limit before authentication (also applied by `Connection::new`).
- `TxStreaming` accepts RUN (another result) and ROLLBACK (which discards the open results); COMMIT is still only accepted once every result is consumed.
- `ChunkWriter::write_message` frames the message in memory and writes it with a single `write_all` (one TLS record per message instead of one write per chunk header), and `ChunkReader` no longer holds a 64 KiB buffer per connection.
- The server moves records out of the buffered result on PULL instead of cloning them.
- The client HELLO user agent is `boltr-client/<crate version>` (it was hard-coded to `0.2.0`).

## [0.2.0] - 2026-04-11

### Added
- `AuthInfo` struct with `principal: String` field, representing the authenticated identity from LOGON validation.
- `BoltBackend::set_session_auth()` default method, called after successful LOGON to pass `AuthInfo` to the backend.

### Changed
- **Breaking**: `AuthValidator::validate()` return type changed from `Result<(), BoltError>` to `Result<AuthInfo, BoltError>`. The `AuthInfo` flows from LOGON validation to the backend via `set_session_auth()`.

## [0.1.2] - 2026-03-14

### Fixed
- **RUSTSEC-2025-0134**: replaced unmaintained `rustls-pemfile` with `rustls-pki-types` for TLS PEM parsing.
- **Unbounded memory allocation**: capped PackStream list/dict pre-allocation to prevent OOM from malicious length declarations.
- **PULL handler panic**: replaced `.unwrap()` with safe pattern match, reject invalid negative `n` values per Bolt spec.
- **LOGOFF session leak**: LOGOFF now rolls back pending transactions and resets the backend session.
- **Idle reaper desync**: connections detect when their session has been reaped and shut down cleanly.
- **TLS accept loop**: fixed `tls_acceptor` moved-in-loop compile error when `tls` feature is enabled.

### Added
- **WebSocket transport** (`ws` feature): Bolt-over-WebSocket support via `tokio-tungstenite`.
  - `WsStream` adapter: wraps `WebSocketStream` as `AsyncRead + AsyncWrite` for seamless integration with existing Bolt framing.
  - `BoltServer::ws_serve(addr)`: standalone Bolt-over-WebSocket server (with WSS when combined with the `tls` feature).
  - `ws::server::accept_ws()` and `handle_ws()`: accept pre-upgraded WebSocket connections for integration with HTTP servers (e.g., Axum).
  - `BoltConnection::connect_ws(url)`: client WebSocket connection supporting `ws://` and `wss://` URLs.
  - `BoltSession::connect_ws()` and `connect_ws_basic()`: high-level client convenience methods.
- **Max message size**: `ChunkReader` enforces a configurable limit (default 16 MiB), preventing multi-GB message accumulation. Exposed via `BoltServer::max_message_size()`.
- `Display` implementations for `ConnectionState`, `ClientMessage`, and `ServerMessage`.
- Crate root re-exports: `BoltError`, `BoltValue`, `BoltBackend`, `BoltServer`, `BoltSession`, and more.
- `#[must_use]` annotations on `BoltValue::as_str()`, `BoltValue::as_int()`, `QueryResult`, and `ConnectionState` transition methods.

### Changed
- `BoltConnection` internals refactored to trait objects (`Box<dyn AsyncRead/Write>`) for transport flexibility.

### Removed
- Vestigial `needless_for_each` clippy lint allow from `Cargo.toml`.

## [0.1.1] - 2026-03-01

### Added
- **ROUTE message** (0x66): Full encode/decode and server-side handler for cluster-aware Neo4j drivers. `RoutingTable` and `RoutingServer` types with `BoltBackend::route()` trait method (default: "not supported").
- **TELEMETRY message** (0x54): Bolt 5.4+ driver telemetry acknowledgment (no-op SUCCESS response).
- **TLS support**: Feature-gated (`tls`) via `tokio-rustls`. `TlsConfig::from_pem()` for certificate loading, `BoltServer::tls()` builder method. Connection handler is stream-generic, so TLS wraps seamlessly.
- **Bookmark utilities**: `extract_bookmarks()` helper to parse bookmarks from Bolt extra dicts. Documented bookmark flow (BEGIN/RUN bookmarks in, COMMIT bookmark out).
- **Client `logoff()`**: Re-authenticate without reconnecting (Bolt 5.1+).
- **Client `pull_n(n)`**: Fetch a specific number of records for batched/incremental pulling.
- **Client `discard_all()`** / **`BoltSession::discard()`**: Skip remaining records in a result stream.
- State machine now accepts `Route` and `Telemetry` in the Ready state.
- Round-trip tests for ROUTE and TELEMETRY messages.

### Changed

- **`BoltConnection::rollback()`** now returns `Result<BoltDict, BoltError>` instead of `Result<(), BoltError>`, symmetric with `commit()`.
- **`BoltSession::commit()`** now returns `Result<BoltDict, BoltError>` instead of `Result<(), BoltError>`, exposing bookmark metadata for causal consistency.
- **`BoltSession::rollback()`** now returns `Result<BoltDict, BoltError>` for symmetry with `commit()`.

### Fixed

- Clippy `approx_constant` warning in PackStream float round-trip test (use `std::f64::consts::PI`).

## [0.1.0] - 2026-02-20

Initial release.

### Added

- Bolt v5.x wire protocol: PackStream encode/decode, chunked transport, message framing.
- Full PackStream type system: all 23 Bolt types including scalars, collections, graph structures, temporal, and spatial types.
- Full message set: HELLO, LOGON, LOGOFF, GOODBYE, RESET, RUN, PULL, DISCARD, BEGIN, COMMIT, ROLLBACK.
- Server framework: `BoltServer` builder with auth, idle timeout, max sessions, and shutdown signal.
- Connection state machine with proper Bolt lifecycle transitions (Negotiation, Authentication, Ready, Streaming, TxReady, TxStreaming, Failed, Defunct).
- Session management with idle reaping.
- `BoltBackend` trait for pluggable server implementations.
- `AuthValidator` trait for pluggable authentication.
- Client library (feature-gated behind `client`): `BoltConnection` for low-level I/O and `BoltSession` for high-level query API.
- Version negotiation supporting Bolt 5.1 through 5.4.
- 61 unit tests covering PackStream encoding, message round-trips, chunk framing, version negotiation, state machine transitions, and session management.
- CI pipeline: formatting, clippy, tests (Linux/Windows/macOS), coverage, and security audit.
- Dual-licensed under MIT and Apache-2.0.

[0.2.1]: https://github.com/GrafeoDB/boltr/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/GrafeoDB/boltr/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/GrafeoDB/boltr/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/GrafeoDB/boltr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/GrafeoDB/boltr/releases/tag/v0.1.0

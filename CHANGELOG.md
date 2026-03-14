# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.2]: https://github.com/GrafeoDB/boltr/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/GrafeoDB/boltr/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/GrafeoDB/boltr/releases/tag/v0.1.0

# Changelog

## 0.1.1

### Added
- **ROUTE message** (0x66): Full encode/decode and server-side handler for cluster-aware Neo4j drivers. `RoutingTable` and `RoutingServer` types with `BoltBackend::route()` trait method (default: "not supported").
- **TELEMETRY message** (0x54): Bolt 5.4+ driver telemetry acknowledgment (no-op SUCCESS response).
- **TLS support**: Feature-gated (`tls`) via `tokio-rustls`. `TlsConfig::from_pem()` for certificate loading, `BoltServer::tls()` builder method. Connection handler is stream-generic, so TLS wraps seamlessly.
- **Bookmark utilities**: `extract_bookmarks()` helper to parse bookmarks from Bolt extra dicts. Documented bookmark flow (BEGIN/RUN bookmarks in, COMMIT bookmark out).
- State machine now accepts `Route` and `Telemetry` in the Ready state.
- Round-trip tests for ROUTE and TELEMETRY messages.

## 0.1.0

Initial release.

- Bolt v5.x wire protocol: PackStream encode/decode, chunked transport, message framing.
- Full message set: HELLO, LOGON, LOGOFF, GOODBYE, RESET, RUN, PULL, DISCARD, BEGIN, COMMIT, ROLLBACK.
- Server framework: `BoltServer` builder with auth, idle timeout, max sessions, shutdown signal.
- Connection state machine with proper Bolt lifecycle transitions.
- Session management with idle reaping.
- `BoltBackend` trait for pluggable server implementations.

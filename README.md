# BoltR

A standalone, pure Rust implementation of the [Bolt v5.x wire protocol](https://neo4j.com/docs/bolt/current/) - the binary protocol used by Neo4j and other graph databases for client-server communication.

Any Bolt-compatible database engine can plug in via the `BoltBackend` trait. BoltR handles transport (TCP, WebSocket, TLS), PackStream encoding, session management, transactions, and the full Bolt type system over the wire.

## Features

- **Spec-faithful:** Full Bolt v5.x protocol (5.1-5.4), all PackStream types, all message types
- **Pure Rust:** No C/C++ dependencies
- **Lightweight:** Minimal deps: tokio, bytes, thiserror, tracing
- **Fast:** Efficient PackStream encoding, chunked streaming
- **Embeddable:** Library-first design, usable by any Rust project
- **WebSocket:** Optional Bolt-over-WebSocket via `ws` feature flag (tokio-tungstenite)
- **TLS:** Optional TLS via `tls` feature flag (tokio-rustls), works with both TCP and WebSocket (WSS)
- **Auth:** Pluggable authentication via `AuthValidator` trait
- **Observability:** Structured tracing via `tracing` crate
- **Graceful shutdown:** Drain connections on signal

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
boltr = "0.1"
```

### Implementing a Backend

Implement the `BoltBackend` trait to connect your database:

```rust
use boltr::server::{BoltBackend, SessionHandle, SessionConfig, ResultStream};
use boltr::error::BoltError;
use boltr::types::BoltDict;

struct MyDatabase { /* ... */ }

#[async_trait::async_trait]
impl BoltBackend for MyDatabase {
    async fn create_session(&self, config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        Ok(SessionHandle("session-1".into()))
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        Ok(())
    }

    // ... implement execute, begin_transaction, commit, rollback, etc.
}
```

### Starting the Server

```rust
use boltr::server::BoltServer;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = MyDatabase::new();
    let addr = "127.0.0.1:7687".parse()?;

    BoltServer::builder(backend)
        .idle_timeout(Duration::from_secs(300))
        .max_sessions(1000)
        .shutdown(async { tokio::signal::ctrl_c().await.unwrap() })
        .serve(addr)
        .await?;

    Ok(())
}
```

### Using the Client

Enable the `client` feature:

```toml
[dependencies]
boltr = { version = "0.1", features = ["client"] }
```

```rust
use boltr::client::BoltSession;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "127.0.0.1:7687".parse()?;
    let mut session = BoltSession::connect(addr).await?;

    let result = session.run("MATCH (n:Person) RETURN n.name").await?;
    println!("columns: {:?}", result.columns);

    for record in &result.records {
        println!("{record:?}");
    }

    session.close().await?;
    Ok(())
}
```

### WebSocket Transport

Enable the `ws` feature for Bolt-over-WebSocket:

```toml
[dependencies]
boltr = { version = "0.1", features = ["client", "ws"] }
```

**Client:**

```rust
use boltr::client::BoltSession;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = BoltSession::connect_ws("ws://127.0.0.1:7688/bolt").await?;
    let result = session.run("RETURN 1 AS n").await?;
    session.close().await?;
    Ok(())
}
```

**Server (standalone):**

```rust
use boltr::server::BoltServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = MyDatabase::new();
    let addr = "127.0.0.1:7688".parse()?;

    BoltServer::builder(backend)
        .shutdown(async { tokio::signal::ctrl_c().await.unwrap() })
        .ws_serve(addr)
        .await?;

    Ok(())
}
```

**Server (pre-upgraded, e.g. Axum):**

```rust
use boltr::ws::server::accept_ws;

// Inside an Axum WebSocket handler:
async fn bolt_ws_handler(ws: WebSocketStream<impl AsyncRead + AsyncWrite + Unpin + Send>) {
    let backend = get_backend();
    accept_ws(ws, backend, config).await;
}
```

## Architecture

```text
Application (Cypher statements, parameters, results)
       |
       v
  Bolt Messages
  - Client: HELLO, LOGON, RUN, PULL, BEGIN, COMMIT, ROLLBACK, GOODBYE
  - Server: SUCCESS, RECORD, FAILURE, IGNORED
       |
       v
  PackStream Encoding
  - Full Bolt type system: scalars, graph elements, temporal, spatial
       |
       v
  Chunk Framing (2-byte length-prefixed)
       |
       v
  Transport (TCP | WebSocket | TLS/WSS)
       |
       v
  BoltBackend trait (your database plugs in here)
```

## Bolt Type Support

| Bolt Type | Wire Encoding |
| --------------------------------------------------------- | -------------------------------------------- |
| `NULL`, `BOOLEAN`, `INTEGER`, `FLOAT`, `STRING`, `BYTES` | PackStream native markers |
| `DATE`, `TIME`, `DATETIME`, `DURATION` | PackStream structures |
| `POINT2D`, `POINT3D` | PackStream structures with SRID |
| `LIST`, `DICT` | Recursive PackStream containers |
| `NODE` | ID + labels + properties + element_id |
| `RELATIONSHIP` | ID + type + start/end + properties + element_id |
| `PATH` | Alternating nodes and relationships |

## Modules

| Module | Description |
| ------------ | ---------------------------------------------------------------- |
| `packstream` | Binary encoding format (markers, encode, decode) |
| `types` | `BoltValue` enum and graph/temporal/spatial types |
| `chunk` | Message framing (length-prefixed chunks) |
| `message` | Client and server message types, encode/decode |
| `server` | `BoltBackend` trait, session/transaction management, TCP server |
| `client` | `BoltConnection`, `BoltSession` (feature-gated with `client`) |
| `ws` | WebSocket adapter and server (feature-gated with `ws`) |
| `error` | `BoltError` enum with Neo4j-compatible codes |

## Feature Flags

| Feature  | Default | Description |
| -------- | ------- | --------------------------------------------- |
| `client` | off | Client library (`BoltConnection`, `BoltSession`) |
| `ws` | off | WebSocket transport (`WsStream`, `ws_serve`) |
| `tls` | off | TLS support via `tokio-rustls` |

Enable all:

```toml
boltr = { version = "0.1", features = ["client", "ws", "tls"] }
```

## Requirements

- Rust 1.85.0+ (edition 2024)

## License

Licensed under either of [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0) or [MIT license](http://opensource.org/licenses/MIT) at your option.

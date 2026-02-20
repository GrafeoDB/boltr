# BoltR

A standalone, pure Rust implementation of the [Bolt v5.x wire protocol](https://neo4j.com/docs/bolt/current/) - the binary protocol used by Neo4j and other graph databases for client-server communication.

Any Bolt-compatible database engine can plug in via the `BoltBackend` trait. BoltR handles TCP transport, PackStream encoding, session management, transactions, and the full Bolt type system over the wire.

## Features

- **Spec-faithful:** Full Bolt v5.x protocol (5.1-5.4), all PackStream types, all message types
- **Pure Rust:** No C/C++ dependencies
- **Lightweight:** Minimal deps: tokio, bytes, thiserror, tracing
- **Fast:** Efficient PackStream encoding, chunked streaming
- **Embeddable:** Library-first design, usable by any Rust project
- **TLS:** Optional TLS via `tls` feature flag (tokio-rustls)
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
use boltr::server::{BoltBackend, SessionHandle, TransactionHandle, SessionConfig, ResultStream, BoltRecord};
use boltr::error::BoltError;

struct MyDatabase { /* ... */ }

#[async_trait::async_trait]
impl BoltBackend for MyDatabase {
    async fn create_session(&self, config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        // Create a session in your database
        Ok(SessionHandle::new("session-1"))
    }

    async fn execute(
        &self,
        session: &SessionHandle,
        query: &str,
        parameters: &std::collections::HashMap<String, boltr::types::BoltValue>,
        transaction: Option<&TransactionHandle>,
    ) -> Result<Box<dyn ResultStream>, BoltError> {
        // Execute query and return a result stream
        todo!()
    }

    // ... other trait methods
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
use boltr::client::BoltConnection;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = BoltConnection::connect("127.0.0.1:7687").await?;
    let mut session = conn.create_session().await?;

    let records = session.run("MATCH (n:Person) RETURN n.name", HashMap::new()).await?;

    for record in records {
        println!("{record:?}");
    }

    session.close().await?;
    Ok(())
}
```

## Architecture

```
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
  BoltBackend trait (your database plugs in here)
```

## Bolt Type Support

| Bolt Type | Wire Encoding |
|-----------|--------------|
| `NULL`, `BOOLEAN`, `INTEGER`, `FLOAT`, `STRING`, `BYTES` | PackStream native markers |
| `DATE`, `TIME`, `DATETIME`, `DURATION` | PackStream structures |
| `POINT2D`, `POINT3D` | PackStream structures with SRID |
| `LIST`, `DICT` | Recursive PackStream containers |
| `NODE` | ID + labels + properties + element_id |
| `RELATIONSHIP` | ID + type + start/end + properties + element_id |
| `PATH` | Alternating nodes and relationships |

## Modules

| Module | Description |
|--------|-------------|
| `packstream` | Binary encoding format (markers, encode, decode) |
| `types` | `BoltValue` enum and graph/temporal/spatial types |
| `chunk` | TCP message framing (length-prefixed chunks) |
| `message` | Client and server message types, encode/decode |
| `server` | `BoltBackend` trait, session/transaction management, TCP server |
| `client` | `BoltConnection`, `BoltSession` (feature-gated) |
| `error` | `BoltError` enum with Neo4j-compatible codes |

## Requirements

- Rust 1.85.0+ (edition 2024)

## License

Licensed under either of [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0) or [MIT license](http://opensource.org/licenses/MIT) at your option.

//! End-to-end tests: a real BoltR server on a TCP socket, driven by a raw Bolt
//! client built from the public framing and message APIs, and (with the
//! `client` feature) by the high-level `BoltSession` client.
//!
//! The mock backend logs every call it receives, so the tests can check what
//! the server did on the backend's side (sessions created and closed,
//! transactions rolled back, queries executed).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::AsyncWriteExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};

use boltr::chunk::{ChunkReader, ChunkWriter};
use boltr::error::BoltError;
use boltr::message::decode::decode_server_message;
use boltr::message::encode::encode_client_message;
use boltr::message::{ClientMessage, ServerMessage};
use boltr::server::handshake::{client_handshake, default_client_proposals};
use boltr::server::{
    AuthCredentials, AuthInfo, AuthValidator, BoltBackend, BoltRecord, BoltServer, ResultMetadata,
    ResultStream, RoutingServer, RoutingTable, SessionConfig, SessionHandle, SessionProperty,
    TransactionHandle,
};
use boltr::types::{
    BoltDate, BoltDateTime, BoltDateTimeZoneId, BoltDict, BoltDuration, BoltLocalDateTime,
    BoltLocalTime, BoltNode, BoltPath, BoltPoint2D, BoltPoint3D, BoltRelationship, BoltTime,
    BoltUnboundRelationship, BoltValue,
};

/// Upper bound for any single wait in these tests.
const TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Mock backend
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockState {
    next_id: AtomicU64,
    events: Mutex<Vec<String>>,
    /// Open sessions: id to authenticated principal ("" until LOGON).
    open: Mutex<HashMap<String, String>>,
}

/// A backend that understands a handful of fake statements:
///
/// - `ROWS n`: `n` records `[i]` (column `i`)
/// - `CREATE ...`: no records, a `stats` summary as grafeo-server sends it
/// - `FAIL`: a query error
/// - `ECHO`: one record holding the parameters dictionary
/// - `WHOAMI`: one record holding the principal set by LOGON
/// - `IN_TX`: one record, whether the statement runs in a transaction
#[derive(Clone, Default)]
struct MockBackend(Arc<MockState>);

impl MockBackend {
    fn log(&self, event: String) {
        self.0.events.lock().unwrap().push(event);
    }

    fn events(&self) -> Vec<String> {
        self.0.events.lock().unwrap().clone()
    }

    fn count(&self, prefix: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| event.starts_with(prefix))
            .count()
    }

    fn open_sessions(&self) -> usize {
        self.0.open.lock().unwrap().len()
    }

    /// Polls until `condition` holds, panicking (with the event log) on timeout.
    async fn wait_until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !condition(self) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}; events: {:#?}",
                self.events()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn rows(n: i64) -> ResultStream {
    ResultStream {
        metadata: ResultMetadata {
            columns: vec!["i".into()],
            extra: BoltDict::new(),
        },
        records: (0..n)
            .map(|i| BoltRecord {
                values: vec![BoltValue::Integer(i)],
            })
            .collect(),
        summary: BoltDict::from([("type".to_string(), BoltValue::String("r".into()))]),
    }
}

fn single(column: &str, value: BoltValue) -> ResultStream {
    ResultStream {
        metadata: ResultMetadata {
            columns: vec![column.into()],
            extra: BoltDict::new(),
        },
        records: vec![BoltRecord {
            values: vec![value],
        }],
        summary: BoltDict::new(),
    }
}

#[async_trait::async_trait]
impl BoltBackend for MockBackend {
    async fn create_session(&self, config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        let id = format!("s{}", self.0.next_id.fetch_add(1, Ordering::SeqCst));
        self.0
            .open
            .lock()
            .unwrap()
            .insert(id.clone(), String::new());
        self.log(format!("create_session {id} {}", config.user_agent));
        Ok(SessionHandle(id))
    }

    async fn set_session_auth(
        &self,
        session: &SessionHandle,
        auth_info: AuthInfo,
    ) -> Result<(), BoltError> {
        if let Some(principal) = self.0.open.lock().unwrap().get_mut(&session.0) {
            principal.clone_from(&auth_info.principal);
        }
        self.log(format!(
            "set_session_auth {} {}",
            session.0, auth_info.principal
        ));
        Ok(())
    }

    async fn close_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        self.0.open.lock().unwrap().remove(&session.0);
        self.log(format!("close_session {}", session.0));
        Ok(())
    }

    async fn configure_session(
        &self,
        session: &SessionHandle,
        property: SessionProperty,
    ) -> Result<(), BoltError> {
        let SessionProperty::Database(db) = property;
        self.log(format!("configure_session {} {db}", session.0));
        Ok(())
    }

    async fn reset_session(&self, session: &SessionHandle) -> Result<(), BoltError> {
        self.log(format!("reset_session {}", session.0));
        Ok(())
    }

    async fn execute(
        &self,
        session: &SessionHandle,
        query: &str,
        parameters: &HashMap<String, BoltValue>,
        _extra: &BoltDict,
        transaction: Option<&TransactionHandle>,
    ) -> Result<ResultStream, BoltError> {
        self.log(format!("execute {} {query}", session.0));
        let words: Vec<&str> = query.split_whitespace().collect();
        match words.as_slice() {
            ["ROWS", n] => Ok(rows(n.parse().expect("row count"))),
            ["CREATE", ..] => Ok(ResultStream {
                metadata: ResultMetadata {
                    columns: vec![],
                    extra: BoltDict::new(),
                },
                records: vec![],
                summary: BoltDict::from([
                    ("type".to_string(), BoltValue::String("w".into())),
                    (
                        "stats".to_string(),
                        BoltValue::Dict(BoltDict::from([
                            ("nodes-created".to_string(), BoltValue::Integer(1)),
                            ("properties-set".to_string(), BoltValue::Integer(2)),
                            ("contains-updates".to_string(), BoltValue::Boolean(true)),
                        ])),
                    ),
                ]),
            }),
            ["ECHO"] => Ok(single("params", BoltValue::Dict(parameters.clone()))),
            ["WHOAMI"] => {
                let principal = self
                    .0
                    .open
                    .lock()
                    .unwrap()
                    .get(&session.0)
                    .cloned()
                    .unwrap_or_default();
                Ok(single("principal", BoltValue::String(principal)))
            }
            ["IN_TX"] => Ok(single("in_tx", BoltValue::Boolean(transaction.is_some()))),
            _ => Err(BoltError::Query {
                code: "Neo.ClientError.Statement.SyntaxError".into(),
                message: format!("mock cannot run {query:?}"),
            }),
        }
    }

    async fn begin_transaction(
        &self,
        session: &SessionHandle,
        _extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError> {
        let id = format!("tx{}", self.0.next_id.fetch_add(1, Ordering::SeqCst));
        self.log(format!("begin {} {id}", session.0));
        Ok(TransactionHandle(id))
    }

    async fn commit(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        self.log(format!("commit {} {}", session.0, transaction.0));
        Ok(BoltDict::from([(
            "bookmark".to_string(),
            BoltValue::String(format!("mock:{}", transaction.0)),
        )]))
    }

    async fn rollback(
        &self,
        session: &SessionHandle,
        transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        self.log(format!("rollback {} {}", session.0, transaction.0));
        Ok(())
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::from([(
            "server".to_string(),
            BoltValue::String("Mock/1.0".into()),
        )]))
    }

    async fn route(
        &self,
        _routing_context: &BoltDict,
        bookmarks: &[String],
        db: Option<&str>,
    ) -> Result<RoutingTable, BoltError> {
        self.log(format!("route {bookmarks:?} {db:?}"));
        Ok(RoutingTable {
            ttl: 300,
            db: db.unwrap_or("neo4j").to_string(),
            servers: ["WRITE", "READ", "ROUTE"]
                .into_iter()
                .map(|role| RoutingServer {
                    addresses: vec!["localhost:7687".into()],
                    role: role.into(),
                })
                .collect(),
        })
    }
}

/// Accepts `basic` auth with password `secret` for any user.
struct PasswordValidator;

#[async_trait::async_trait]
impl AuthValidator for PasswordValidator {
    async fn validate(&self, credentials: &AuthCredentials) -> Result<AuthInfo, BoltError> {
        match (
            credentials.scheme.as_str(),
            credentials.principal.as_deref(),
            credentials.credentials.as_deref(),
        ) {
            ("basic", Some(user), Some("secret")) => Ok(AuthInfo {
                principal: user.to_string(),
                credentials_expired: false,
            }),
            _ => Err(BoltError::Authentication("invalid credentials".into())),
        }
    }
}

type Configure = fn(BoltServer<MockBackend>) -> BoltServer<MockBackend>;

/// Starts a server on an ephemeral port.
async fn start_server(configure: Configure) -> (SocketAddr, MockBackend) {
    let backend = MockBackend::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = configure(BoltServer::builder(backend.clone()));
    tokio::spawn(async move { server.serve_listener(listener).await });
    (addr, backend)
}

async fn start_default_server() -> (SocketAddr, MockBackend) {
    start_server(|server| server).await
}

// ---------------------------------------------------------------------------
// Raw client
// ---------------------------------------------------------------------------

/// A minimal Bolt client that sends exactly what a test asks for.
struct RawClient {
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
}

impl RawClient {
    /// Connects and performs the version handshake.
    async fn connect(addr: SocketAddr) -> Self {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let version = client_handshake(&mut stream, &default_client_proposals())
            .await
            .unwrap();
        assert_eq!(version, (5, 4));
        Self::from_stream(stream)
    }

    fn from_stream(stream: TcpStream) -> Self {
        let (read, write) = stream.into_split();
        Self { read, write }
    }

    /// Connects, then HELLO + LOGON (scheme "none").
    async fn login(addr: SocketAddr) -> Self {
        let mut client = Self::connect(addr).await;
        client.send(&hello()).await;
        client.success().await;
        client.send(&logon_none()).await;
        client.success().await;
        client
    }

    async fn try_send(&mut self, msg: &ClientMessage) -> Result<(), BoltError> {
        let mut payload = BytesMut::new();
        encode_client_message(&mut payload, msg);
        ChunkWriter::new(&mut self.write)
            .write_message(&payload)
            .await
    }

    async fn send(&mut self, msg: &ClientMessage) {
        self.try_send(msg).await.unwrap();
    }

    /// Frames `payload` as one chunked message.
    async fn send_payload(&mut self, payload: &[u8]) {
        ChunkWriter::new(&mut self.write)
            .write_message(payload)
            .await
            .unwrap();
    }

    /// Writes bytes as-is (no framing).
    async fn send_raw(&mut self, bytes: &[u8]) {
        self.write.write_all(bytes).await.unwrap();
    }

    async fn read_one(&mut self) -> Result<Option<ServerMessage>, BoltError> {
        loop {
            let data = ChunkReader::new(&mut self.read).read_message().await?;
            if !data.is_empty() {
                return decode_server_message(&data).map(Some);
            }
        }
    }

    /// The next message, or `None` once the server has closed the connection.
    async fn recv_or_closed(&mut self) -> Option<ServerMessage> {
        let result = tokio::time::timeout(TIMEOUT, self.read_one())
            .await
            .expect("timed out waiting for the server");
        match result {
            Ok(message) => message,
            Err(BoltError::Io(_)) => None,
            Err(e) => panic!("undecodable server message: {e}"),
        }
    }

    async fn recv(&mut self) -> ServerMessage {
        self.recv_or_closed()
            .await
            .expect("connection closed unexpectedly")
    }

    async fn success(&mut self) -> BoltDict {
        match self.recv().await {
            ServerMessage::Success { metadata } => metadata,
            other => panic!("expected SUCCESS, got {other:?}"),
        }
    }

    /// Expects a FAILURE and returns its code.
    async fn failure(&mut self) -> String {
        match self.recv().await {
            ServerMessage::Failure { metadata } => metadata
                .get("code")
                .and_then(BoltValue::as_str)
                .unwrap_or_default()
                .to_string(),
            other => panic!("expected FAILURE, got {other:?}"),
        }
    }

    async fn ignored(&mut self) {
        match self.recv().await {
            ServerMessage::Ignored => {}
            other => panic!("expected IGNORED, got {other:?}"),
        }
    }

    /// Collects RECORDs up to the closing SUCCESS.
    async fn records(&mut self) -> (Vec<Vec<BoltValue>>, BoltDict) {
        let mut records = Vec::new();
        loop {
            match self.recv().await {
                ServerMessage::Record { data } => records.push(data),
                ServerMessage::Success { metadata } => return (records, metadata),
                other => panic!("expected RECORD or SUCCESS, got {other:?}"),
            }
        }
    }

    async fn expect_closed(&mut self) {
        if let Some(message) = self.recv_or_closed().await {
            panic!("expected the server to close the connection, got {message:?}");
        }
    }

    /// Runs `query` and pulls everything.
    async fn query(&mut self, query: &str) -> (Vec<Vec<BoltValue>>, BoltDict) {
        self.send(&run(query)).await;
        self.success().await;
        self.send(&ClientMessage::pull_all()).await;
        self.records().await
    }
}

fn hello() -> ClientMessage {
    ClientMessage::Hello {
        extra: BoltDict::from([(
            "user_agent".to_string(),
            BoltValue::String("e2e/1.0".into()),
        )]),
    }
}

fn logon_none() -> ClientMessage {
    ClientMessage::Logon {
        auth: BoltDict::from([("scheme".to_string(), BoltValue::String("none".into()))]),
    }
}

fn logon_basic(user: &str, password: &str) -> ClientMessage {
    ClientMessage::Logon {
        auth: BoltDict::from([
            ("scheme".to_string(), BoltValue::String("basic".into())),
            ("principal".to_string(), BoltValue::String(user.into())),
            (
                "credentials".to_string(),
                BoltValue::String(password.into()),
            ),
        ]),
    }
}

fn run(query: &str) -> ClientMessage {
    run_with(query, BoltDict::new())
}

fn run_with(query: &str, parameters: BoltDict) -> ClientMessage {
    ClientMessage::Run {
        query: query.into(),
        parameters,
        extra: BoltDict::new(),
    }
}

fn discard(n: i64) -> ClientMessage {
    ClientMessage::Discard {
        extra: BoltDict::from([("n".to_string(), BoltValue::Integer(n))]),
    }
}

fn begin() -> ClientMessage {
    ClientMessage::Begin {
        extra: BoltDict::new(),
    }
}

fn has_more(metadata: &BoltDict) -> bool {
    match metadata.get("has_more") {
        Some(BoltValue::Boolean(b)) => *b,
        other => panic!("has_more missing or not a boolean: {other:?}"),
    }
}

fn ints(records: &[Vec<BoltValue>]) -> Vec<i64> {
    records
        .iter()
        .map(|r| r[0].as_int().expect("integer column"))
        .collect()
}

// ---------------------------------------------------------------------------
// Full sessions
// ---------------------------------------------------------------------------

/// HELLO, LOGON, RUN with PULL paging, explicit transactions (COMMIT and
/// ROLLBACK), ROUTE, TELEMETRY, RESET and GOODBYE over a real socket.
#[tokio::test]
async fn full_session_over_tcp() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::connect(addr).await;

    client.send(&hello()).await;
    let meta = client.success().await;
    assert_eq!(
        meta.get("server"),
        Some(&BoltValue::String("Mock/1.0".into()))
    );
    assert!(matches!(
        meta.get("connection_id"),
        Some(BoltValue::String(_))
    ));
    assert!(matches!(meta.get("hints"), Some(BoltValue::Dict(_))));

    client.send(&logon_none()).await;
    client.success().await;

    // Auto-commit RUN with paging: 5 rows in batches of 2.
    client.send(&run("ROWS 5")).await;
    let meta = client.success().await;
    assert_eq!(
        meta.get("fields"),
        Some(&BoltValue::List(vec![BoltValue::String("i".into())]))
    );
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        client.send(&ClientMessage::pull_n(2)).await;
        let (records, meta) = client.records().await;
        assert!(records.len() <= 2);
        seen.extend(ints(&records));
        pages += 1;
        if !has_more(&meta) {
            // The summary arrives with the last page only.
            assert_eq!(meta.get("type"), Some(&BoltValue::String("r".into())));
            break;
        }
        assert!(!meta.contains_key("type"));
    }
    assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    assert_eq!(pages, 3);

    // Explicit transaction, committed.
    client.send(&begin()).await;
    client.success().await;
    let (records, summary) = client.query("CREATE (n)").await;
    assert!(records.is_empty());
    assert!(matches!(summary.get("stats"), Some(BoltValue::Dict(_))));
    let (records, _) = client.query("IN_TX").await;
    assert_eq!(records, vec![vec![BoltValue::Boolean(true)]]);
    client.send(&ClientMessage::Commit).await;
    let meta = client.success().await;
    assert!(matches!(meta.get("bookmark"), Some(BoltValue::String(b)) if b.starts_with("mock:tx")));

    // Explicit transaction, rolled back.
    client.send(&begin()).await;
    client.success().await;
    client.query("CREATE (n)").await;
    client.send(&ClientMessage::Rollback).await;
    client.success().await;

    // Back in auto-commit mode.
    let (records, _) = client.query("IN_TX").await;
    assert_eq!(records, vec![vec![BoltValue::Boolean(false)]]);

    // ROUTE.
    client
        .send(&ClientMessage::Route {
            routing: BoltDict::from([(
                "address".to_string(),
                BoltValue::String("localhost:7687".into()),
            )]),
            bookmarks: vec!["bk:1".into()],
            extra: BoltDict::from([("db".to_string(), BoltValue::String("neo4j".into()))]),
        })
        .await;
    let meta = client.success().await;
    let Some(BoltValue::Dict(rt)) = meta.get("rt") else {
        panic!("ROUTE SUCCESS without rt: {meta:?}");
    };
    assert_eq!(rt.get("ttl"), Some(&BoltValue::Integer(300)));
    assert_eq!(rt.get("db"), Some(&BoltValue::String("neo4j".into())));
    let Some(BoltValue::List(servers)) = rt.get("servers") else {
        panic!("rt without servers: {rt:?}");
    };
    assert_eq!(servers.len(), 3);

    // TELEMETRY.
    client.send(&ClientMessage::Telemetry { api: 1 }).await;
    client.success().await;

    // RESET in READY.
    client.send(&ClientMessage::Reset).await;
    client.success().await;

    // GOODBYE closes the connection and the backend session.
    client.send(&ClientMessage::Goodbye).await;
    client.expect_closed().await;
    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
    assert_eq!(backend.open_sessions(), 0);
    assert_eq!(backend.count("commit"), 1);
    assert_eq!(backend.count("rollback"), 1);
    assert_eq!(backend.count("route"), 1);
    assert_eq!(backend.count("create_session s0 e2e/1.0"), 1);
}

/// Drivers pipeline HELLO, LOGON, RUN and PULL before reading anything.
#[tokio::test]
async fn pipelined_messages() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::connect(addr).await;
    for msg in [
        hello(),
        logon_none(),
        run("ROWS 3"),
        ClientMessage::pull_all(),
        run("ROWS 1"),
        ClientMessage::pull_all(),
    ] {
        client.send(&msg).await;
    }
    client.success().await;
    client.success().await;
    client.success().await;
    let (records, meta) = client.records().await;
    assert_eq!(ints(&records), vec![0, 1, 2]);
    assert!(!has_more(&meta));
    client.success().await;
    let (records, _) = client.records().await;
    assert_eq!(ints(&records), vec![0]);
}

/// Regression: DISCARD ignored `n` and dropped the result summary, so a
/// driver calling `consume()` on a partly read result lost its `stats` and
/// bookmark.
#[tokio::test]
async fn discard_honours_n_and_returns_the_summary() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    client.send(&run("ROWS 5")).await;
    client.success().await;
    client.send(&discard(2)).await;
    let meta = client.success().await;
    assert!(has_more(&meta));
    client.send(&ClientMessage::pull_all()).await;
    let (records, meta) = client.records().await;
    assert_eq!(ints(&records), vec![2, 3, 4]);
    assert!(!has_more(&meta));

    // DISCARD everything of a write: the summary carries the stats.
    client.send(&run("CREATE (n)")).await;
    client.success().await;
    client.send(&ClientMessage::discard_all()).await;
    let meta = client.success().await;
    assert!(!has_more(&meta));
    let Some(BoltValue::Dict(stats)) = meta.get("stats") else {
        panic!("DISCARD summary without stats: {meta:?}");
    };
    assert_eq!(stats.get("nodes-created"), Some(&BoltValue::Integer(1)));

    // Back in READY: a new RUN is accepted.
    let (records, _) = client.query("ROWS 1").await;
    assert_eq!(records.len(), 1);
}

#[tokio::test]
async fn pull_larger_than_remaining_and_huge_n() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    client.send(&run("ROWS 3")).await;
    client.success().await;
    client.send(&ClientMessage::pull_n(i64::MAX)).await;
    let (records, meta) = client.records().await;
    assert_eq!(records.len(), 3);
    assert!(!has_more(&meta));

    // An empty result completes on the first PULL.
    client.send(&run("ROWS 0")).await;
    client.success().await;
    client.send(&ClientMessage::pull_n(1)).await;
    let (records, meta) = client.records().await;
    assert!(records.is_empty());
    assert!(!has_more(&meta));
}

#[tokio::test]
async fn invalid_pull_sizes_fail_and_reset_recovers() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    for n in [0, -2, i64::MIN] {
        client.send(&run("ROWS 3")).await;
        client.success().await;
        client.send(&ClientMessage::pull_n(n)).await;
        assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
        // FAILED: everything but RESET is ignored.
        client.send(&ClientMessage::pull_all()).await;
        client.ignored().await;
        client.send(&ClientMessage::Reset).await;
        client.success().await;
    }

    let (records, _) = client.query("ROWS 2").await;
    assert_eq!(records.len(), 2);
}

/// Every PackStream type survives the trip client to server to client.
#[tokio::test]
async fn every_value_type_round_trips_through_the_server() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    let node = BoltNode {
        id: 1,
        labels: vec!["Person".into(), "Émigré".into()],
        properties: BoltDict::from([("name".to_string(), BoltValue::String("Ada".into()))]),
        element_id: "4:db:1".into(),
    };
    let params = BoltDict::from([
        ("null".to_string(), BoltValue::Null),
        ("bool".to_string(), BoltValue::Boolean(true)),
        ("int".to_string(), BoltValue::Integer(i64::MIN)),
        ("float".to_string(), BoltValue::Float(-0.5)),
        ("string".to_string(), BoltValue::String("x".repeat(70_000))),
        ("bytes".to_string(), BoltValue::Bytes(vec![0, 255, 7])),
        (
            "list".to_string(),
            BoltValue::List(vec![BoltValue::Integer(1), BoltValue::List(vec![])]),
        ),
        ("node".to_string(), BoltValue::Node(node.clone())),
        (
            "rel".to_string(),
            BoltValue::Relationship(BoltRelationship {
                id: 9,
                start_node_id: 1,
                end_node_id: 2,
                rel_type: "KNOWS".into(),
                properties: BoltDict::new(),
                element_id: "5:db:9".into(),
                start_element_id: "4:db:1".into(),
                end_element_id: "4:db:2".into(),
            }),
        ),
        (
            "path".to_string(),
            BoltValue::Path(BoltPath {
                nodes: vec![node.clone(), node],
                rels: vec![BoltUnboundRelationship {
                    id: 9,
                    rel_type: "KNOWS".into(),
                    properties: BoltDict::new(),
                    element_id: "5:db:9".into(),
                }],
                indices: vec![1, 1],
            }),
        ),
        ("date".to_string(), BoltValue::Date(BoltDate { days: -1 })),
        (
            "time".to_string(),
            BoltValue::Time(BoltTime {
                nanoseconds: 1,
                tz_offset_seconds: -3600,
            }),
        ),
        (
            "local_time".to_string(),
            BoltValue::LocalTime(BoltLocalTime { nanoseconds: 2 }),
        ),
        (
            "datetime".to_string(),
            BoltValue::DateTime(BoltDateTime {
                seconds: 3,
                nanoseconds: 4,
                tz_offset_seconds: 7200,
            }),
        ),
        (
            "datetime_zone".to_string(),
            BoltValue::DateTimeZoneId(BoltDateTimeZoneId {
                seconds: 5,
                nanoseconds: 6,
                tz_id: "Europe/Amsterdam".into(),
            }),
        ),
        (
            "local_datetime".to_string(),
            BoltValue::LocalDateTime(BoltLocalDateTime {
                seconds: 7,
                nanoseconds: 8,
            }),
        ),
        (
            "duration".to_string(),
            BoltValue::Duration(BoltDuration {
                months: 1,
                days: 2,
                seconds: 3,
                nanoseconds: 4,
            }),
        ),
        (
            "point2d".to_string(),
            BoltValue::Point2D(BoltPoint2D {
                srid: 4326,
                x: 1.5,
                y: 2.5,
            }),
        ),
        (
            "point3d".to_string(),
            BoltValue::Point3D(BoltPoint3D {
                srid: 4979,
                x: 1.0,
                y: 2.0,
                z: 3.0,
            }),
        ),
    ]);

    client.send(&run_with("ECHO", params.clone())).await;
    client.success().await;
    client.send(&ClientMessage::pull_all()).await;
    let (records, _) = client.records().await;
    assert_eq!(records, vec![vec![BoltValue::Dict(params)]]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_connections() {
    let (addr, backend) = start_default_server().await;
    let tasks: Vec<_> = (0..32)
        .map(|i| {
            tokio::spawn(async move {
                let mut client = RawClient::login(addr).await;
                let (records, _) = client.query(&format!("ROWS {i}")).await;
                assert_eq!(records.len(), i);
                client.send(&ClientMessage::Goodbye).await;
                client.expect_closed().await;
            })
        })
        .collect();
    for task in tasks {
        task.await.unwrap();
    }
    backend
        .wait_until("all sessions closed", |b| b.open_sessions() == 0)
        .await;
    assert_eq!(backend.count("create_session"), 32);
    assert_eq!(backend.count("close_session"), 32);
}

// ---------------------------------------------------------------------------
// Authentication and state machine
// ---------------------------------------------------------------------------

fn with_password_auth(server: BoltServer<MockBackend>) -> BoltServer<MockBackend> {
    server.auth(PasswordValidator)
}

#[tokio::test]
async fn basic_auth_success_reaches_the_backend() {
    let (addr, backend) = start_server(with_password_auth).await;
    let mut client = RawClient::connect(addr).await;
    client.send(&hello()).await;
    client.success().await;
    client.send(&logon_basic("ada", "secret")).await;
    client.success().await;
    let (records, _) = client.query("WHOAMI").await;
    assert_eq!(records, vec![vec![BoltValue::String("ada".into())]]);
    assert_eq!(backend.count("set_session_auth s0 ada"), 1);
}

/// Regression (authentication bypass): a failed LOGON left the connection
/// in FAILED, and RESET then moved it to READY without authentication, so
/// the next RUN executed against the backend session.
#[tokio::test]
async fn failed_logon_closes_the_connection() {
    let (addr, backend) = start_server(with_password_auth).await;
    let mut client = RawClient::connect(addr).await;
    client.send(&hello()).await;
    client.success().await;
    client.send(&logon_basic("ada", "wrong")).await;
    assert_eq!(
        client.failure().await,
        "Neo.ClientError.Security.Unauthorized"
    );

    // The bypass attempt: RESET then RUN. Neither may be answered.
    let _ = client.try_send(&ClientMessage::Reset).await;
    let _ = client.try_send(&run("ROWS 1")).await;
    let _ = client.try_send(&ClientMessage::pull_all()).await;
    client.expect_closed().await;

    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
    assert_eq!(backend.count("execute"), 0, "{:#?}", backend.events());
    assert_eq!(backend.count("set_session_auth"), 0);
    assert_eq!(backend.open_sessions(), 0);
}

/// Regression (authentication bypass): an undecodable message before LOGON
/// moved the connection to FAILED, from which RESET reached READY.
#[tokio::test]
async fn malformed_message_before_logon_closes_the_connection() {
    let (addr, backend) = start_server(with_password_auth).await;
    let mut client = RawClient::connect(addr).await;
    client.send(&hello()).await;
    client.success().await;

    client.send_payload(&[0xB1, 0x99, 0xC0]).await; // unknown signature
    assert_eq!(
        client.failure().await,
        "Neo.ClientError.Request.InvalidFormat"
    );
    let _ = client.try_send(&ClientMessage::Reset).await;
    let _ = client.try_send(&run("ROWS 1")).await;
    client.expect_closed().await;

    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
    assert_eq!(backend.count("execute"), 0);
}

#[tokio::test]
async fn messages_before_logon_are_rejected_and_close_the_connection() {
    let (addr, backend) = start_server(with_password_auth).await;

    let premature = [
        run("ROWS 1"),
        begin(),
        ClientMessage::Reset,
        ClientMessage::pull_all(),
        ClientMessage::Logoff,
        ClientMessage::Telemetry { api: 0 },
        ClientMessage::Route {
            routing: BoltDict::new(),
            bookmarks: vec![],
            extra: BoltDict::new(),
        },
    ];

    // Before HELLO (no session yet), and after HELLO but before LOGON.
    for after_hello in [false, true] {
        for msg in &premature {
            let mut client = RawClient::connect(addr).await;
            if after_hello {
                client.send(&hello()).await;
                client.success().await;
            }
            client.send(msg).await;
            assert_eq!(
                client.failure().await,
                "Neo.ClientError.Request.Invalid",
                "{msg} (after HELLO: {after_hello})"
            );
            client.expect_closed().await;
        }
    }

    // LOGON before HELLO is rejected the same way.
    let mut client = RawClient::connect(addr).await;
    client.send(&logon_basic("ada", "secret")).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.expect_closed().await;

    backend
        .wait_until("sessions closed", |b| b.open_sessions() == 0)
        .await;
    assert_eq!(backend.count("execute"), 0);
    assert_eq!(backend.count("begin"), 0);
}

#[tokio::test]
async fn goodbye_before_hello_closes_quietly() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::connect(addr).await;
    client.send(&ClientMessage::Goodbye).await;
    client.expect_closed().await;
    assert_eq!(backend.count("create_session"), 0);
}

#[tokio::test]
async fn logoff_requires_a_new_logon() {
    let (addr, backend) = start_server(with_password_auth).await;
    let mut client = RawClient::connect(addr).await;
    client.send(&hello()).await;
    client.success().await;
    client.send(&logon_basic("ada", "secret")).await;
    client.success().await;

    // Re-authenticate as someone else on the same connection.
    client.send(&ClientMessage::Logoff).await;
    client.success().await;
    client.send(&logon_basic("grace", "secret")).await;
    client.success().await;
    let (records, _) = client.query("WHOAMI").await;
    assert_eq!(records, vec![vec![BoltValue::String("grace".into())]]);

    // After LOGOFF, RUN is rejected and the connection closed.
    client.send(&ClientMessage::Logoff).await;
    client.success().await;
    client.send(&run("ROWS 1")).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.expect_closed().await;
    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
}

#[tokio::test]
async fn logoff_rolls_back_an_open_transaction() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;
    client.send(&begin()).await;
    client.success().await;
    client.query("CREATE (n)").await;
    // LOGOFF is only valid in READY, so it fails inside a transaction.
    client.send(&ClientMessage::Logoff).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.send(&ClientMessage::Reset).await;
    client.success().await;
    assert_eq!(backend.count("rollback"), 1);
}

#[tokio::test]
async fn out_of_state_messages_fail_and_reset_recovers() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    // (setup messages, offending message) pairs, each starting from READY.
    let cases: Vec<(Vec<ClientMessage>, ClientMessage)> = vec![
        (vec![], ClientMessage::pull_all()),
        (vec![], ClientMessage::discard_all()),
        (vec![], ClientMessage::Commit),
        (vec![], ClientMessage::Rollback),
        (vec![], logon_none()),
        (vec![], hello()),
        (vec![run("ROWS 2")], run("ROWS 1")),
        (vec![run("ROWS 2")], begin()),
        (vec![run("ROWS 2")], ClientMessage::Commit),
        (vec![begin()], begin()),
        (vec![begin()], ClientMessage::Telemetry { api: 0 }),
        (vec![begin(), run("ROWS 2")], ClientMessage::Commit),
        (vec![begin(), run("ROWS 2")], begin()),
    ];

    for (setup, offending) in cases {
        for msg in &setup {
            client.send(msg).await;
            client.success().await;
        }
        client.send(&offending).await;
        assert_eq!(
            client.failure().await,
            "Neo.ClientError.Request.Invalid",
            "{offending} after {setup:?}"
        );
        // Now FAILED: other messages are IGNORED until RESET.
        client.send(&run("ROWS 1")).await;
        client.ignored().await;
        client.send(&ClientMessage::Reset).await;
        client.success().await;
        let (records, _) = client.query("ROWS 1").await;
        assert_eq!(records.len(), 1, "recovery after {offending}");
    }
}

#[tokio::test]
async fn query_failure_ignores_pipelined_messages_until_reset() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;
    for msg in [
        run("FAIL"),
        ClientMessage::pull_all(),
        run("ROWS 1"),
        ClientMessage::pull_all(),
        ClientMessage::Reset,
        run("ROWS 1"),
        ClientMessage::pull_all(),
    ] {
        client.send(&msg).await;
    }
    assert_eq!(
        client.failure().await,
        "Neo.ClientError.Statement.SyntaxError"
    );
    client.ignored().await;
    client.ignored().await;
    client.ignored().await;
    client.success().await;
    client.success().await;
    let (records, _) = client.records().await;
    assert_eq!(records.len(), 1);
}

/// RESET works in every authenticated state and rolls back open transactions.
#[tokio::test]
async fn reset_in_every_authenticated_state() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    let setups: Vec<(&str, Vec<ClientMessage>, usize)> = vec![
        ("READY", vec![], 0),
        ("STREAMING", vec![run("ROWS 3")], 0),
        ("TX_READY", vec![begin()], 1),
        ("TX_STREAMING", vec![begin(), run("ROWS 3")], 1),
    ];
    let mut expected_rollbacks = 0;
    for (state, setup, rollbacks) in setups {
        for msg in &setup {
            client.send(msg).await;
            client.success().await;
        }
        client.send(&ClientMessage::Reset).await;
        client.success().await;
        expected_rollbacks += rollbacks;
        assert_eq!(backend.count("rollback"), expected_rollbacks, "{state}");
        let (records, _) = client.query("IN_TX").await;
        assert_eq!(records, vec![vec![BoltValue::Boolean(false)]], "{state}");
    }

    // FAILED.
    client.send(&run("FAIL")).await;
    client.failure().await;
    client.send(&ClientMessage::Reset).await;
    client.success().await;
    let (records, _) = client.query("ROWS 1").await;
    assert_eq!(records.len(), 1);
}

#[tokio::test]
async fn goodbye_in_any_state_closes_and_cleans_up() {
    let (addr, backend) = start_default_server().await;
    let setups: Vec<Vec<ClientMessage>> = vec![
        vec![],
        vec![run("ROWS 3")],
        vec![begin()],
        vec![begin(), run("ROWS 3")],
        vec![run("FAIL")],
    ];
    for (i, setup) in setups.iter().enumerate() {
        let mut client = RawClient::login(addr).await;
        for msg in setup {
            client.send(msg).await;
            client.recv().await;
        }
        client.send(&ClientMessage::Goodbye).await;
        client.expect_closed().await;
        backend
            .wait_until("session close", |b| b.count("close_session") == i + 1)
            .await;
    }
    // The two connections that were inside a transaction rolled it back.
    assert_eq!(backend.count("rollback"), 2);
    assert_eq!(backend.open_sessions(), 0);
}

// ---------------------------------------------------------------------------
// Several open results per transaction (qid)
// ---------------------------------------------------------------------------

fn pull_qid(n: i64, qid: i64) -> ClientMessage {
    ClientMessage::Pull {
        extra: BoltDict::from([
            ("n".to_string(), BoltValue::Integer(n)),
            ("qid".to_string(), BoltValue::Integer(qid)),
        ]),
    }
}

fn discard_qid(n: i64, qid: i64) -> ClientMessage {
    ClientMessage::Discard {
        extra: BoltDict::from([
            ("n".to_string(), BoltValue::Integer(n)),
            ("qid".to_string(), BoltValue::Integer(qid)),
        ]),
    }
}

fn qid_of(metadata: &BoltDict) -> i64 {
    metadata
        .get("qid")
        .and_then(BoltValue::as_int)
        .unwrap_or_else(|| panic!("RUN SUCCESS without qid: {metadata:?}"))
}

/// Two results open in one transaction, pulled and discarded in an
/// interleaved order, as drivers do once a result exceeds the fetch size.
#[tokio::test]
async fn transaction_interleaves_results_by_qid() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    client.send(&begin()).await;
    client.success().await;

    // First result: RUN + PULL pipelined, as drivers do (PULL uses qid -1).
    client.send(&run("ROWS 5")).await;
    client.send(&ClientMessage::pull_n(2)).await;
    let first = qid_of(&client.success().await);
    assert_eq!(first, 0);
    let (records, meta) = client.records().await;
    assert_eq!(ints(&records), vec![0, 1]);
    assert!(has_more(&meta));

    // Second result while the first is still open.
    client.send(&run("ROWS 3")).await;
    let second = qid_of(&client.success().await);
    assert_eq!(second, 1);

    // Interleave: one more from the first, everything from the second.
    client.send(&pull_qid(1, first)).await;
    let (records, meta) = client.records().await;
    assert_eq!(ints(&records), vec![2]);
    assert!(has_more(&meta));

    client.send(&pull_qid(-1, second)).await;
    let (records, meta) = client.records().await;
    assert_eq!(ints(&records), vec![0, 1, 2]);
    assert!(!has_more(&meta));
    assert_eq!(meta.get("type"), Some(&BoltValue::String("r".into())));

    // The first result is still open; a third one can start anyway.
    client.send(&run("CREATE (n)")).await;
    let third = qid_of(&client.success().await);
    assert_eq!(third, 2);
    client.send(&ClientMessage::discard_all()).await; // qid -1: the CREATE
    let meta = client.success().await;
    assert!(!has_more(&meta));
    assert!(matches!(meta.get("stats"), Some(BoltValue::Dict(_))));

    // Discard the rest of the first result: no result is open any more.
    client.send(&discard_qid(-1, first)).await;
    let meta = client.success().await;
    assert!(!has_more(&meta));

    client.send(&ClientMessage::Commit).await;
    let meta = client.success().await;
    assert!(meta.contains_key("bookmark"));
    assert_eq!(backend.count("commit"), 1);

    // qids restart in the next transaction; auto-commit RUN has none.
    let (records, _) = client.query("ROWS 1").await;
    assert_eq!(records.len(), 1);
    client.send(&run("ROWS 1")).await;
    assert!(!client.success().await.contains_key("qid"));
    client.send(&ClientMessage::pull_all()).await;
    client.records().await;

    client.send(&begin()).await;
    client.success().await;
    client.send(&run("ROWS 1")).await;
    assert_eq!(qid_of(&client.success().await), 0);
}

#[tokio::test]
async fn commit_waits_for_open_results_but_rollback_does_not() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    client.send(&begin()).await;
    client.success().await;
    client.send(&run("ROWS 3")).await;
    client.success().await;
    client.send(&run("ROWS 3")).await;
    client.success().await;

    // ROLLBACK with two results open ends the transaction.
    client.send(&ClientMessage::Rollback).await;
    client.success().await;
    assert_eq!(backend.count("rollback"), 1);
    client.send(&ClientMessage::pull_all()).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.send(&ClientMessage::Reset).await;
    client.success().await;

    // COMMIT with an open result is rejected.
    client.send(&begin()).await;
    client.success().await;
    client.send(&run("ROWS 3")).await;
    client.success().await;
    client.send(&ClientMessage::Commit).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    assert_eq!(backend.count("commit"), 0);
}

#[tokio::test]
async fn invalid_qids_are_rejected() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    let bad: Vec<(&str, ClientMessage)> = vec![
        ("unknown qid", pull_qid(-1, 7)),
        ("negative qid", pull_qid(-1, -2)),
        ("unknown qid", discard_qid(-1, 7)),
        (
            "string qid",
            ClientMessage::Pull {
                extra: BoltDict::from([("qid".to_string(), BoltValue::String("0".into()))]),
            },
        ),
    ];
    for (what, msg) in bad {
        client.send(&begin()).await;
        client.success().await;
        client.send(&run("ROWS 2")).await;
        client.success().await;
        client.send(&msg).await;
        assert_eq!(
            client.failure().await,
            "Neo.ClientError.Request.Invalid",
            "{what}"
        );
        client.send(&ClientMessage::Reset).await;
        client.success().await;
    }

    // qid -1 once the most recent result is finished, another still open.
    client.send(&begin()).await;
    client.success().await;
    client.send(&run("ROWS 2")).await;
    client.success().await;
    client.send(&run("ROWS 1")).await;
    client.success().await;
    client.send(&ClientMessage::pull_all()).await;
    client.records().await;
    client.send(&ClientMessage::pull_all()).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
}

// ---------------------------------------------------------------------------
// Message size before authentication
// ---------------------------------------------------------------------------

fn hello_with_padding(bytes: usize) -> ClientMessage {
    ClientMessage::Hello {
        extra: BoltDict::from([("padding".to_string(), BoltValue::String("x".repeat(bytes)))]),
    }
}

/// Regression: before LOGON a client could send a full-size (16 MiB)
/// message whose decoded form needs gigabytes. Unauthenticated messages are
/// now limited (64 KiB by default) and the full limit applies after LOGON.
#[tokio::test]
async fn messages_before_logon_have_a_lower_size_limit() {
    let (addr, backend) = start_default_server().await;

    // A large HELLO is refused and the connection closed.
    let mut client = RawClient::connect(addr).await;
    client.send(&hello_with_padding(100 * 1024)).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.expect_closed().await;
    assert_eq!(backend.count("create_session"), 0);

    // A HELLO just under the limit is fine.
    let mut client = RawClient::connect(addr).await;
    client.send(&hello_with_padding(60 * 1024)).await;
    client.success().await;
    client.send(&logon_none()).await;
    client.success().await;

    // After LOGON the general limit applies.
    let params = BoltDict::from([("p".to_string(), BoltValue::String("y".repeat(300_000)))]);
    client.send(&run_with("ECHO", params.clone())).await;
    client.success().await;
    client.send(&ClientMessage::pull_all()).await;
    let (records, _) = client.records().await;
    assert_eq!(records, vec![vec![BoltValue::Dict(params)]]);

    // After LOGOFF the lower limit is back.
    client.send(&ClientMessage::Logoff).await;
    client.success().await;
    client
        .send(&ClientMessage::Logon {
            auth: BoltDict::from([
                ("scheme".to_string(), BoltValue::String("none".into())),
                (
                    "padding".to_string(),
                    BoltValue::String("z".repeat(100 * 1024)),
                ),
            ]),
        })
        .await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.expect_closed().await;
}

#[tokio::test]
async fn unauthenticated_message_size_is_configurable() {
    let (addr, _backend) =
        start_server(|server| server.max_unauthenticated_message_size(1024 * 1024)).await;
    let mut client = RawClient::connect(addr).await;
    client.send(&hello_with_padding(200 * 1024)).await;
    client.success().await;

    // It never exceeds the general limit.
    let (addr, _backend) = start_server(|server| {
        server
            .max_unauthenticated_message_size(1024 * 1024)
            .max_message_size(4096)
    })
    .await;
    let mut client = RawClient::connect(addr).await;
    client.send(&hello_with_padding(8192)).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.expect_closed().await;
}

// ---------------------------------------------------------------------------
// Malformed input over the socket
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oversized_message_gets_a_failure_and_the_connection_closes() {
    let (addr, backend) = start_server(|server| server.max_message_size(1024)).await;
    let mut client = RawClient::login(addr).await;

    let big = run_with(
        "ECHO",
        BoltDict::from([("x".to_string(), BoltValue::String("y".repeat(4096)))]),
    );
    client.send(&big).await;
    assert_eq!(client.failure().await, "Neo.ClientError.Request.Invalid");
    client.expect_closed().await;
    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
    assert_eq!(backend.count("execute"), 0);
}

/// Regression: deeply nested parameters overflowed the decoder's stack and
/// aborted the server process.
#[tokio::test]
async fn deeply_nested_parameters_fail_without_crashing_the_server() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    // RUN "ECHO" {p: [[[[...]]]]} {}
    let mut payload = vec![0xB3, 0x10, 0x84, b'E', b'C', b'H', b'O', 0xA1, 0x81, b'p'];
    payload.extend(std::iter::repeat_n(0x91, 500_000));
    payload.push(0xC0);
    payload.push(0xA0);
    client.send_payload(&payload).await;
    assert_eq!(
        client.failure().await,
        "Neo.ClientError.Request.InvalidFormat"
    );
    client.send(&ClientMessage::Reset).await;
    client.success().await;
    let (records, _) = client.query("ROWS 1").await;
    assert_eq!(records.len(), 1);

    // And the server still accepts new connections.
    let mut other = RawClient::login(addr).await;
    other.query("ROWS 1").await;
}

#[tokio::test]
async fn malformed_messages_after_logon_fail_and_reset_recovers() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    let payloads: &[&[u8]] = &[
        &[0x01],                                     // not a structure
        &[0xB0],                                     // missing signature
        &[0xB0, 0x77],                               // unknown signature
        &[0xB3, 0x10, 0x81],                         // truncated RUN
        &[0xB3, 0x10, 0x01, 0xA0, 0xA0],             // RUN with integer query
        &[0xB1, 0x3F, 0xD6, 0xFF, 0xFF, 0xFF, 0xFF], // PULL extra: LIST_32 lie
        &[0xB1, 0x3F, 0xA1, 0x01, 0x01],             // PULL extra with an integer key
        &[0xB1, 0x3F, 0xC7],                         // reserved marker
        &[0xB1, 0x3F, 0xB0, 0x44],                   // Date structure without fields
    ];
    for payload in payloads {
        client.send_payload(payload).await;
        assert_eq!(
            client.failure().await,
            "Neo.ClientError.Request.InvalidFormat",
            "{payload:02X?}"
        );
        client.send(&ClientMessage::Reset).await;
        client.success().await;
    }
    let (records, _) = client.query("ROWS 2").await;
    assert_eq!(records.len(), 2);
    assert_eq!(backend.count("execute"), 1);
}

#[tokio::test]
async fn noop_chunks_between_messages_are_ignored() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::connect(addr).await;
    client.send_raw(&[0x00, 0x00, 0x00, 0x00]).await;
    client.send(&hello()).await;
    client.send_raw(&[0x00, 0x00]).await;
    client.send(&logon_none()).await;
    client.success().await;
    client.success().await;
    client.send_raw(&[0x00, 0x00]).await;
    let (records, _) = client.query("ROWS 2").await;
    assert_eq!(records.len(), 2);
}

/// A message (and the values inside it) split into one-byte chunks, plus a
/// 70 000 byte string spanning the 65 535 byte chunk limit.
#[tokio::test]
async fn messages_split_across_chunks_are_reassembled() {
    let (addr, _backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;

    let mut payload = BytesMut::new();
    let params = BoltDict::from([
        ("s".to_string(), BoltValue::String("é".repeat(35_000))),
        ("n".to_string(), BoltValue::Integer(-1_000_000_000_000)),
    ]);
    encode_client_message(&mut payload, &run_with("ECHO", params.clone()));
    assert!(payload.len() > 65_535);

    // Head in one-byte chunks (splits markers, sizes and UTF-8 sequences),
    // the rest in maximum-size chunks.
    let mut framed = Vec::new();
    for byte in &payload[..64] {
        framed.extend_from_slice(&[0x00, 0x01, *byte]);
    }
    for chunk in payload[64..].chunks(65_535) {
        framed.extend_from_slice(&u16::try_from(chunk.len()).unwrap().to_be_bytes());
        framed.extend_from_slice(chunk);
    }
    framed.extend_from_slice(&[0x00, 0x00]);
    client.send_raw(&framed).await;
    client.success().await;
    client.send(&ClientMessage::pull_all()).await;
    let (records, _) = client.records().await;
    assert_eq!(records, vec![vec![BoltValue::Dict(params)]]);
}

#[tokio::test]
async fn truncated_message_then_disconnect_cleans_up() {
    let (addr, backend) = start_default_server().await;
    let mut client = RawClient::login(addr).await;
    client.send(&begin()).await;
    client.success().await;
    // A chunk header promising 100 bytes, 3 bytes, then the client leaves.
    client.send_raw(&[0x00, 0x64, 0xB3, 0x10, 0x81]).await;
    drop(client);
    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
    assert_eq!(backend.count("rollback"), 1);
    assert_eq!(backend.open_sessions(), 0);
}

/// Regression: when a write failed (client gone mid-stream) the handler
/// returned early and skipped cleanup, leaking the session. With a session
/// limit, a few such disconnects locked every client out.
#[tokio::test]
async fn disconnect_mid_stream_releases_the_session() {
    let (addr, backend) = start_server(|server| server.max_sessions(1)).await;

    let mut client = RawClient::login(addr).await;
    client.send(&run("ROWS 500000")).await;
    client.success().await;
    client.send(&ClientMessage::pull_all()).await;
    assert!(matches!(client.recv().await, ServerMessage::Record { .. }));
    drop(client);

    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
    assert_eq!(backend.open_sessions(), 0);

    // The single session slot is free again.
    let mut client = RawClient::login(addr).await;
    let (records, _) = client.query("ROWS 1").await;
    assert_eq!(records.len(), 1);
}

/// Regression: when the session limit rejected a HELLO, the backend session
/// created for it was never closed.
#[tokio::test]
async fn session_limit_rejection_does_not_leak_backend_sessions() {
    let (addr, backend) = start_server(|server| server.max_sessions(1)).await;
    let _first = RawClient::login(addr).await;

    let mut second = RawClient::connect(addr).await;
    second.send(&hello()).await;
    assert_eq!(
        second.failure().await,
        "Neo.TransientError.General.MemoryPoolOutOfMemoryError"
    );
    second.expect_closed().await;

    backend
        .wait_until("rejected session closed", |b| {
            b.count("close_session s1") == 1
        })
        .await;
    assert_eq!(backend.open_sessions(), 1);
}

#[tokio::test]
async fn garbage_instead_of_messages_never_crashes_the_server() {
    let (addr, _backend) = start_default_server().await;

    // Deterministic pseudo-random bytes (xorshift).
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..20 {
        let mut client = RawClient::connect(addr).await;
        let len = 1 + usize::try_from(state % 300).unwrap();
        let mut garbage = Vec::with_capacity(len);
        for _ in 0..len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            garbage.push(state.to_le_bytes()[0]);
        }
        client.send_raw(&garbage).await;
        drop(client);
    }

    // Still serving.
    let mut client = RawClient::login(addr).await;
    let (records, _) = client.query("ROWS 1").await;
    assert_eq!(records.len(), 1);
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handshake_with_unsupported_versions_gets_no_version() {
    let (addr, _backend) = start_default_server().await;
    for proposals in [
        [0u8; 16],
        [0, 0, 4, 4, 0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 1],
        [0xFF; 16],
    ] {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let result = client_handshake(&mut stream, &proposals).await;
        assert!(
            result.unwrap_err().to_string().contains("rejected"),
            "{proposals:02X?}"
        );
    }
}

#[tokio::test]
async fn bad_magic_closes_without_a_response() {
    let (addr, _backend) = start_default_server().await;
    let stream = TcpStream::connect(addr).await.unwrap();
    let mut client = RawClient::from_stream(stream);
    client.send_raw(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
    client.expect_closed().await;
}

fn with_short_handshake_timeout(server: BoltServer<MockBackend>) -> BoltServer<MockBackend> {
    server.handshake_timeout(Duration::from_millis(300))
}

#[tokio::test]
async fn silent_connections_are_closed_by_the_handshake_timeout() {
    let (addr, backend) = start_server(with_short_handshake_timeout).await;

    // Never sends the handshake.
    let stream = TcpStream::connect(addr).await.unwrap();
    let mut silent = RawClient::from_stream(stream);
    silent.expect_closed().await;

    // Handshake only.
    let mut handshake_only = RawClient::connect(addr).await;
    handshake_only.expect_closed().await;

    // HELLO but no LOGON: the session is released too.
    let mut no_logon = RawClient::connect(addr).await;
    no_logon.send(&hello()).await;
    no_logon.success().await;
    no_logon.expect_closed().await;
    backend
        .wait_until("session close", |b| b.count("close_session") == 1)
        .await;
}

#[tokio::test]
async fn authenticated_connections_outlive_the_handshake_timeout() {
    let (addr, _backend) = start_server(with_short_handshake_timeout).await;
    let mut client = RawClient::login(addr).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (records, _) = client.query("ROWS 1").await;
    assert_eq!(records.len(), 1);

    // After LOGOFF the clock restarts.
    client.send(&ClientMessage::Logoff).await;
    client.success().await;
    client.expect_closed().await;
}

#[tokio::test]
async fn zero_handshake_timeout_disables_it() {
    let (addr, _backend) = start_server(|server| server.handshake_timeout(Duration::ZERO)).await;
    let mut client = RawClient::connect(addr).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    client.send(&hello()).await;
    client.success().await;
}

// ---------------------------------------------------------------------------
// Idle sessions
// ---------------------------------------------------------------------------

/// Regression: only RUN refreshed a session's activity, so a client paging
/// through a result (or sending only RESETs) was reaped while active.
#[tokio::test]
async fn every_message_keeps_a_session_alive() {
    let (addr, backend) =
        start_server(|server| server.idle_timeout(Duration::from_millis(300))).await;
    let mut client = RawClient::login(addr).await;
    client.send(&run("ROWS 20")).await;
    client.success().await;
    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        client.send(&ClientMessage::pull_n(1)).await;
        let (records, _) = client.records().await;
        assert_eq!(records.len(), 1);
    }
    assert_eq!(backend.count("close_session"), 0);
}

#[tokio::test]
async fn idle_sessions_are_reaped() {
    let (addr, backend) =
        start_server(|server| server.idle_timeout(Duration::from_millis(100))).await;
    let mut client = RawClient::login(addr).await;
    backend
        .wait_until("idle session reaped", |b| b.count("close_session") == 1)
        .await;
    // The connection notices at its next message and closes.
    let _ = client.try_send(&run("ROWS 1")).await;
    client.expect_closed().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(backend.count("close_session"), 1, "closed twice");
}

/// Regression: a zero idle timeout made the reaper's `interval` panic, so no
/// session was ever reaped.
#[tokio::test]
async fn zero_idle_timeout_does_not_panic_the_reaper() {
    let (addr, backend) = start_server(|server| server.idle_timeout(Duration::ZERO)).await;
    // Every session is idle for "longer than zero", so it is reaped right
    // after HELLO; a working reaper is all this test checks.
    let mut client = RawClient::connect(addr).await;
    client.send(&hello()).await;
    client.success().await;
    backend
        .wait_until("session reaped", |b| b.count("close_session") == 1)
        .await;
}

// ---------------------------------------------------------------------------
// High-level client
// ---------------------------------------------------------------------------

#[cfg(feature = "client")]
mod client_api {
    use super::*;
    use boltr::client::{BoltConnection, BoltSession, Counters};

    #[tokio::test]
    async fn bolt_session_full_flow() {
        let (addr, backend) = start_default_server().await;
        let mut session = BoltSession::connect(addr).await.unwrap();
        assert_eq!(session.version(), (5, 4));

        let result = session.run("ROWS 3").await.unwrap();
        assert_eq!(result.columns, vec!["i".to_string()]);
        assert_eq!(ints(&result.records), vec![0, 1, 2]);
        assert_eq!(result.counters(), Counters::default());
        assert!(!result.counters().contains_updates());

        let result = session.run("CREATE (n)").await.unwrap();
        let counters = result.counters();
        assert_eq!(counters.nodes_created, 1);
        assert_eq!(counters.properties_set, 2);
        assert!(counters.contains_updates());

        session.begin().await.unwrap();
        let _ = session.run("CREATE (n)").await.unwrap();
        let commit = session.commit().await.unwrap();
        assert!(matches!(commit.get("bookmark"), Some(BoltValue::String(_))));

        session.begin().await.unwrap();
        session.rollback().await.unwrap();

        assert!(session.run("FAIL").await.is_err());
        session.reset().await.unwrap();
        assert_eq!(session.run("ROWS 1").await.unwrap().records.len(), 1);

        session.close().await.unwrap();
        backend
            .wait_until("session close", |b| b.count("close_session") == 1)
            .await;
        assert!(
            backend
                .events()
                .iter()
                .any(|e| e.contains(concat!("boltr-client/", env!("CARGO_PKG_VERSION"))))
        );
    }

    #[tokio::test]
    async fn connection_paging_and_discard() {
        let (addr, _backend) = start_default_server().await;
        let mut conn = BoltConnection::connect(addr).await.unwrap();
        conn.hello(BoltDict::new()).await.unwrap();
        conn.logon("none", None, None).await.unwrap();

        conn.run("ROWS 5", HashMap::new(), BoltDict::new())
            .await
            .unwrap();
        let (records, meta) = conn.pull_n(3).await.unwrap();
        assert_eq!(records.len(), 3);
        assert!(has_more(&meta));
        conn.discard_all().await.unwrap();

        conn.run("ROWS 2", HashMap::new(), BoltDict::new())
            .await
            .unwrap();
        let (records, meta) = conn.pull_all().await.unwrap();
        assert_eq!(records.len(), 2);
        assert!(!has_more(&meta));

        conn.logoff().await.unwrap();
        conn.logon("none", None, None).await.unwrap();
        conn.goodbye().await.unwrap();
    }

    #[tokio::test]
    async fn basic_auth_rejection_surfaces_as_an_error() {
        let (addr, _backend) = start_server(with_password_auth).await;
        assert!(
            BoltSession::connect_basic(addr, "ada", "wrong")
                .await
                .is_err()
        );
        let mut session = BoltSession::connect_basic(addr, "ada", "secret")
            .await
            .unwrap();
        let result = session.run("WHOAMI").await.unwrap();
        assert_eq!(result.records, vec![vec![BoltValue::String("ada".into())]]);
    }

    /// A scripted server: performs the handshake, then answers each request
    /// with the given raw responses.
    async fn scripted_server(handshake_answer: [u8; 4], responses: Vec<Vec<u8>>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut read, mut write) = stream.into_split();
            let mut request = [0u8; 20];
            tokio::io::AsyncReadExt::read_exact(&mut read, &mut request)
                .await
                .unwrap();
            write.write_all(&handshake_answer).await.unwrap();
            for response in responses {
                // One request in, one scripted response out.
                if ChunkReader::new(&mut read).read_message().await.is_err() {
                    return;
                }
                write.write_all(&response).await.unwrap();
            }
            // Keep the socket open until the client is done.
            let _ = ChunkReader::new(&mut read).read_message().await;
        });
        addr
    }

    fn framed(msg: &ServerMessage) -> Vec<u8> {
        let mut payload = BytesMut::new();
        boltr::message::encode::encode_server_message(&mut payload, msg);
        let mut out = Vec::new();
        out.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    /// Regression: the client decoded NOOP keep-alive chunks (which Neo4j
    /// sends during long-running queries) as messages and failed.
    #[tokio::test]
    async fn client_skips_noop_keepalives() {
        let noop = vec![0x00, 0x00];
        let success = |meta: BoltDict| framed(&ServerMessage::Success { metadata: meta });
        let fields = BoltDict::from([(
            "fields".to_string(),
            BoltValue::List(vec![BoltValue::String("x".into())]),
        )]);
        let mut pull_response = noop.clone();
        pull_response.extend(framed(&ServerMessage::Record {
            data: vec![BoltValue::Integer(42)],
        }));
        pull_response.extend(&noop);
        pull_response.extend(&noop);
        pull_response.extend(success(BoltDict::new()));

        let responses = vec![
            [noop.clone(), success(BoltDict::new())].concat(), // HELLO
            success(BoltDict::new()),                          // LOGON
            [noop.clone(), success(fields)].concat(),          // RUN
            pull_response,                                     // PULL
        ];
        let addr = scripted_server([0, 0, 4, 5], responses).await;
        let mut session = BoltSession::connect(addr).await.unwrap();
        let result = session.run("RETURN 42 AS x").await.unwrap();
        assert_eq!(result.records, vec![vec![BoltValue::Integer(42)]]);
    }

    #[tokio::test]
    async fn client_rejects_a_non_bolt_server() {
        let addr = scripted_server(*b"HTTP", vec![]).await;
        let err = BoltConnection::connect(addr)
            .await
            .err()
            .expect("must fail");
        assert!(err.to_string().contains("not proposed"), "{err}");
    }

    #[cfg(feature = "ws")]
    #[tokio::test]
    async fn websocket_session() {
        let backend = MockBackend::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = BoltServer::builder(backend.clone());
        tokio::spawn(async move { server.ws_serve_listener(listener).await });

        let mut session = BoltSession::connect_ws(&format!("ws://{addr}/"))
            .await
            .unwrap();
        let result = session.run("ROWS 4").await.unwrap();
        assert_eq!(ints(&result.records), vec![0, 1, 2, 3]);
        session.close().await.unwrap();
        backend
            .wait_until("session close", |b| b.count("close_session") == 1)
            .await;
    }
}

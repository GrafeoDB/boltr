//! Memory an unauthenticated client can make the server allocate.
//!
//! A decoded PackStream value takes about 170 bytes per one-byte encoded
//! value, so before the pre-authentication message limit a single HELLO of a
//! few megabytes cost hundreds of megabytes (up to gigabytes at the 16 MiB
//! message limit). This test measures peak heap use with a counting global
//! allocator, so it lives in its own test binary with a single test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use boltr::chunk::{ChunkReader, ChunkWriter};
use boltr::error::BoltError;
use boltr::message::ServerMessage;
use boltr::message::decode::decode_server_message;
use boltr::packstream::marker;
use boltr::server::handshake::{client_handshake, default_client_proposals};
use boltr::server::{
    BoltBackend, BoltServer, ResultMetadata, ResultStream, SessionConfig, SessionHandle,
    SessionProperty, TransactionHandle,
};
use boltr::types::{BoltDict, BoltValue};

/// Counts live heap bytes and the high-water mark.
struct CountingAllocator;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(bytes: usize) {
    let now = CURRENT.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

fn shrink(bytes: usize) {
    CURRENT.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: every call is forwarded unchanged to the system allocator; the
// wrapper only updates counters.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as `GlobalAlloc::alloc`.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as `GlobalAlloc::alloc_zeroed`.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            grow(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as `GlobalAlloc::dealloc`.
        unsafe { System.dealloc(ptr, layout) };
        shrink(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: same contract as `GlobalAlloc::realloc`.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            // Count the new block before releasing the old one: a moving
            // realloc briefly holds both.
            grow(new_size);
            shrink(layout.size());
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct NullBackend;

#[async_trait::async_trait]
impl BoltBackend for NullBackend {
    async fn create_session(&self, _config: &SessionConfig) -> Result<SessionHandle, BoltError> {
        Ok(SessionHandle("s".into()))
    }

    async fn close_session(&self, _session: &SessionHandle) -> Result<(), BoltError> {
        Ok(())
    }

    async fn configure_session(
        &self,
        _session: &SessionHandle,
        _property: SessionProperty,
    ) -> Result<(), BoltError> {
        Ok(())
    }

    async fn reset_session(&self, _session: &SessionHandle) -> Result<(), BoltError> {
        Ok(())
    }

    async fn execute(
        &self,
        _session: &SessionHandle,
        _query: &str,
        _parameters: &HashMap<String, BoltValue>,
        _extra: &BoltDict,
        _transaction: Option<&TransactionHandle>,
    ) -> Result<ResultStream, BoltError> {
        Ok(ResultStream {
            metadata: ResultMetadata {
                columns: vec![],
                extra: BoltDict::new(),
            },
            records: vec![],
            summary: BoltDict::new(),
        })
    }

    async fn begin_transaction(
        &self,
        _session: &SessionHandle,
        _extra: &BoltDict,
    ) -> Result<TransactionHandle, BoltError> {
        Ok(TransactionHandle("t".into()))
    }

    async fn commit(
        &self,
        _session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::new())
    }

    async fn rollback(
        &self,
        _session: &SessionHandle,
        _transaction: &TransactionHandle,
    ) -> Result<(), BoltError> {
        Ok(())
    }

    async fn get_server_info(&self) -> Result<BoltDict, BoltError> {
        Ok(BoltDict::new())
    }
}

/// A chunk-framed HELLO whose extra holds one list of `items` tiny ints:
/// one byte on the wire each, about 170 bytes each once decoded.
fn crafted_hello(items: u32) -> Vec<u8> {
    let mut payload = vec![0xB1, 0x01, 0xA1, 0x81, b'x', marker::LIST_32];
    payload.extend_from_slice(&items.to_be_bytes());
    payload.resize(payload.len() + items as usize, 0x01);
    let mut framed = Vec::new();
    for chunk in payload.chunks(65_535) {
        framed.extend_from_slice(&u16::try_from(chunk.len()).unwrap().to_be_bytes());
        framed.extend_from_slice(chunk);
    }
    framed.extend_from_slice(&[0x00, 0x00]);
    framed
}

/// Sends `message` before authenticating and returns the server's answer
/// and the peak heap growth (bytes) while it was handled.
async fn peak_while_handling(
    server: BoltServer<NullBackend>,
    message: Vec<u8>,
) -> (ServerMessage, usize) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server.serve_listener(listener));

    let mut stream = TcpStream::connect(addr).await.unwrap();
    client_handshake(&mut stream, &default_client_proposals())
        .await
        .unwrap();
    let (mut read, mut write) = stream.into_split();

    let baseline = CURRENT.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);

    let writer = tokio::spawn(async move {
        // The server may close after reading part of the message.
        let _ = write.write_all(&message).await;
        // Hand the buffer back instead of freeing it here: freeing it during
        // the measurement would hide the server's allocations.
        (write, message)
    });
    let data = ChunkReader::new(&mut read).read_message().await.unwrap();
    let answer = decode_server_message(&data).unwrap();
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);

    let (mut write, message) = writer.await.unwrap();
    drop(message);
    let _ = ChunkWriter::new(&mut write)
        .write_message(&[0xB0, 0x02])
        .await; // GOODBYE
    (answer, peak)
}

#[tokio::test]
async fn unauthenticated_clients_cannot_force_large_allocations() {
    const MIB: usize = 1024 * 1024;

    // 4 MiB HELLO: rejected by the 64 KiB pre-authentication limit.
    let (answer, peak) = peak_while_handling(
        BoltServer::builder(NullBackend),
        crafted_hello(4 * 1024 * 1024),
    )
    .await;
    assert!(
        matches!(answer, ServerMessage::Failure { .. }),
        "expected FAILURE, got {answer:?}"
    );
    assert!(
        peak < 8 * MIB,
        "rejected HELLO raised peak heap by {} MiB",
        peak / MIB
    );

    // Control: with the limit lifted to the general 16 MiB, a 1 MiB HELLO is
    // decoded and costs well over 100 MiB. This is what the limit prevents,
    // and shows the measurement above would catch a regression.
    let (answer, peak) = peak_while_handling(
        BoltServer::builder(NullBackend).max_unauthenticated_message_size(16 * MIB),
        crafted_hello(1024 * 1024),
    )
    .await;
    assert!(
        matches!(answer, ServerMessage::Success { .. }),
        "expected SUCCESS, got {answer:?}"
    );
    assert!(
        peak > 100 * MIB,
        "decoding a 1 MiB list only raised peak heap by {} MiB",
        peak / MIB
    );
}

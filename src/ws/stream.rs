//! Adapter that wraps a `WebSocketStream` as `AsyncRead + AsyncWrite`.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BytesMut;
use futures_util::Sink;
use futures_util::stream::Stream;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

/// Adapts a [`WebSocketStream`] into a byte-oriented `AsyncRead + AsyncWrite`
/// stream suitable for Bolt protocol framing.
///
/// Binary WebSocket messages are buffered and served as a contiguous byte
/// stream. Text frames are rejected (Bolt is a binary protocol). Ping/pong
/// frames are handled transparently by tungstenite.
pub struct WsStream<S> {
    inner: WebSocketStream<S>,
    read_buf: BytesMut,
    write_buf: BytesMut,
    read_closed: bool,
}

impl<S> WsStream<S> {
    /// Wraps an already-upgraded `WebSocketStream`.
    pub fn new(inner: WebSocketStream<S>) -> Self {
        Self {
            inner,
            read_buf: BytesMut::new(),
            write_buf: BytesMut::new(),
            read_closed: false,
        }
    }
}

impl<S> AsyncRead for WsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // Drain any buffered bytes first.
        if !this.read_buf.is_empty() {
            let to_copy = this.read_buf.len().min(buf.remaining());
            buf.put_slice(&this.read_buf.split_to(to_copy));
            return Poll::Ready(Ok(()));
        }

        if this.read_closed {
            return Poll::Ready(Ok(()));
        }

        // Poll the WebSocket for the next message.
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(msg))) => match msg {
                Message::Binary(data) => {
                    let to_copy = data.len().min(buf.remaining());
                    buf.put_slice(&data[..to_copy]);
                    if to_copy < data.len() {
                        this.read_buf.extend_from_slice(&data[to_copy..]);
                    }
                    Poll::Ready(Ok(()))
                }
                Message::Close(_) => {
                    this.read_closed = true;
                    Poll::Ready(Ok(()))
                }
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                    // Ping/pong handled by tungstenite; wake to poll again.
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Message::Text(_) => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Bolt requires binary WebSocket frames, received text",
                ))),
            },
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionAborted, e)))
            }
            Poll::Ready(None) => {
                this.read_closed = true;
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> AsyncWrite for WsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.write_buf.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        if !this.write_buf.is_empty() {
            // Ensure the sink is ready to accept a message.
            match Pin::new(&mut this.inner).poll_ready(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, e)));
                }
                Poll::Pending => return Poll::Pending,
            }

            let data = this.write_buf.split().freeze().to_vec();
            if let Err(e) = Pin::new(&mut this.inner).start_send(Message::Binary(data.into())) {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, e)));
            }
        }

        // Flush the underlying WebSocket sink.
        Pin::new(&mut this.inner)
            .poll_flush(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // Send a close frame.
        match Pin::new(&mut this.inner).poll_ready(cx) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, e)));
            }
            Poll::Pending => return Poll::Pending,
        }

        let _ = Pin::new(&mut this.inner).start_send(Message::Close(None));

        Pin::new(&mut this.inner)
            .poll_close(cx)
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }
}

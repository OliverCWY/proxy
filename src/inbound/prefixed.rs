use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A stream that first yields a pre-buffered byte slice, then delegates all I/O to the underlying stream.
pub struct PrefixedStream<S> {
    prefix: Bytes,
    prefix_cursor: usize,
    inner: S,
}

impl<S> PrefixedStream<S> {
    pub fn new(prefix: Bytes, inner: S) -> Self {
        Self {
            prefix,
            prefix_cursor: 0,
            inner,
        }
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let prefix_len = self.prefix.len();
        if self.prefix_cursor < prefix_len {
            let available = prefix_len - self.prefix_cursor;
            let to_copy = available.min(buf.remaining());
            buf.put_slice(&self.prefix[self.prefix_cursor..self.prefix_cursor + to_copy]);
            self.prefix_cursor += to_copy;
            return Poll::Ready(Ok(()));
        }

        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

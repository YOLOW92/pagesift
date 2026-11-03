//! `pagesift_rustls::ClientConnection` 接到 tokio 的传输上。默认路径的 tokio-rustls 只认原版 rustls，
//! 这里只要客户端的那部分，写法和 tokio-rustls 的 `common::Stream` 相同。

use std::io::{self, Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use pagesift_rustls::ClientConnection;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub(crate) struct TlsStream<T> {
    io: T,
    conn: ClientConnection,
    eof: bool,
    closing: bool,
}

/// 把 `poll_*` 包成同步 `Read`、`Write`，`Pending` 变 `WouldBlock`，rustls 只认这一层。
struct SyncIo<'a, 'b, T> {
    io: &'a mut T,
    cx: &'a mut Context<'b>,
}

impl<T: AsyncRead + Unpin> Read for SyncIo<'_, '_, T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut buf = ReadBuf::new(buf);
        match Pin::new(&mut *self.io).poll_read(self.cx, &mut buf) {
            Poll::Ready(Ok(())) => Ok(buf.filled().len()),
            Poll::Ready(Err(error)) => Err(error),
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

impl<T: AsyncWrite + Unpin> Write for SyncIo<'_, '_, T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match Pin::new(&mut *self.io).poll_write(self.cx, buf) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match Pin::new(&mut *self.io).poll_flush(self.cx) {
            Poll::Ready(result) => result,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

fn pending_if_blocked(result: io::Result<usize>) -> Poll<io::Result<usize>> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
        other => Poll::Ready(other),
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> TlsStream<T> {
    /// 握手做完才返回：证书校验失败在这里报。
    pub(crate) async fn connect(io: T, conn: ClientConnection) -> io::Result<Self> {
        let mut stream = Self {
            io,
            conn,
            eof: false,
            closing: false,
        };
        std::future::poll_fn(|cx| stream.poll_handshake(cx)).await?;
        Ok(stream)
    }

    pub(crate) fn alpn_protocol(&self) -> Option<&[u8]> {
        self.conn.alpn_protocol()
    }

    fn poll_handshake(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            let mut blocked = false;
            while self.conn.wants_write() {
                match self.write_io(cx) {
                    Poll::Ready(Ok(0)) => return Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
                    Poll::Ready(Ok(_)) => {}
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => {
                        blocked = true;
                        break;
                    }
                }
            }
            if !self.conn.is_handshaking() && !self.conn.wants_write() {
                return Poll::Ready(Ok(()));
            }
            if self.conn.is_handshaking() && self.conn.wants_read() {
                match self.read_io(cx) {
                    Poll::Ready(Ok(0)) => {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "connection closed during TLS handshake",
                        )));
                    }
                    Poll::Ready(Ok(_)) => continue,
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => blocked = true,
                }
            }
            if blocked {
                return Poll::Pending;
            }
        }
    }

    fn write_io(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut io = SyncIo {
            io: &mut self.io,
            cx,
        };
        pending_if_blocked(self.conn.write_tls(&mut io))
    }

    fn read_io(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        let mut io = SyncIo {
            io: &mut self.io,
            cx,
        };
        let n = ready!(pending_if_blocked(self.conn.read_tls(&mut io)))?;
        if let Err(error) = self.conn.process_new_packets() {
            // 这是解不了的错误：尽量把告警发出去，发不出去也不盖住原来的错误
            let _ = self.write_io(cx);
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, error)));
        }
        Poll::Ready(Ok(n))
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncRead for TlsStream<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut blocked = false;
        while !this.eof && this.conn.wants_read() {
            match this.read_io(cx) {
                Poll::Ready(Ok(0)) => {
                    this.eof = true;
                    break;
                }
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => {
                    blocked = true;
                    break;
                }
            }
        }
        // 读的时候可能产生了要发的东西（KeyUpdate 的回应）
        while this.conn.wants_write() {
            match this.write_io(cx) {
                Poll::Ready(Ok(n)) if n > 0 => {}
                _ => break,
            }
        }
        match this.conn.reader().read(buf.initialize_unfilled()) {
            Ok(n) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if !blocked {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> AsyncWrite for TlsStream<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let mut pos = 0;
        while pos != buf.len() {
            pos += this.conn.writer().write(&buf[pos..])?;
            let mut blocked = false;
            while this.conn.wants_write() {
                match this.write_io(cx) {
                    Poll::Ready(Ok(0)) | Poll::Pending => {
                        blocked = true;
                        break;
                    }
                    Poll::Ready(Ok(_)) => {}
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                }
            }
            if blocked {
                return if pos == 0 {
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(pos))
                };
            }
        }
        Poll::Ready(Ok(pos))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.conn.writer().flush()?;
        while this.conn.wants_write() {
            if ready!(this.write_io(cx))? == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
        }
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.closing {
            this.conn.send_close_notify();
            this.closing = true;
        }
        while this.conn.wants_write() {
            if ready!(this.write_io(cx))? == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
        }
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}

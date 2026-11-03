//! h2、hyper 和传输之间的字节层。它们自己写不出来的只有这几处：
//!
//! - HTTP/2：伪头部顺序（h2 固定 m,s,a,p，Chrome 是 m,a,s,p）和 HEADERS 帧的优先级；
//! - HTTP/1.1：请求头名字的大小写（Chrome 的 `sec-ch-*` 小写，其他首字母大写）。
//!
//! 客户端这里只发 GET，没有请求体，所以写出去的只有请求头和 HTTP/2 控制帧。
//! HPACK 不重新编码：`:scheme https` 是静态表索引（1 字节），不写动态表，
//! 把它和 `:authority` 的表示对调不影响任何后续字段的索引。

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::profile::Http2Settings;

/// [协议] RFC 9113 §3.4 的连接前言长度、§4.1 的帧头长度和帧类型、标志。
const PREFACE_BYTES: usize = 24;
const FRAME_HEADER_BYTES: usize = 9; // [协议] RFC 9113、RFC 7541
const FRAME_HEADERS: u8 = 0x1;
const FRAME_CONTINUATION: u8 = 0x9; // [协议] RFC 9113、RFC 7541
const FLAG_END_HEADERS: u8 = 0x4; // [协议] RFC 9113、RFC 7541
const FLAG_PADDED: u8 = 0x8; // [协议] RFC 9113、RFC 7541
const FLAG_PRIORITY: u8 = 0x20; // [协议] RFC 9113、RFC 7541
const PRIORITY_BYTES: usize = 5; // [协议] RFC 9113、RFC 7541
/// [协议] RFC 9113 §6.5.2：对方不声明时 SETTINGS_MAX_FRAME_SIZE 的初始值，任何对端都接受。
const MIN_MAX_FRAME: usize = 16384;
/// [协议] RFC 7541 附录 A：`:scheme http`、`:scheme https` 的静态表索引表示。
const SCHEME_HTTP: u8 = 0x86;
const SCHEME_HTTPS: u8 = 0x87; // [协议] RFC 9113、RFC 7541

enum Mode {
    Http1,
    Http2 {
        preface_left: usize,
        dependency: u32,
        weight: u8,
    },
}

pub(crate) struct ShapedIo<T> {
    inner: T,
    mode: Mode,
    /// 还没凑成完整帧或完整请求头的字节。
    pending: Vec<u8>,
    /// 改好、等着写给传输的字节。
    out: Vec<u8>,
    written: usize,
}

impl<T> ShapedIo<T> {
    pub(crate) fn http1(inner: T) -> Self {
        Self::new(inner, Mode::Http1)
    }

    pub(crate) fn http2(inner: T, settings: &Http2Settings) -> Self {
        Self::new(
            inner,
            Mode::Http2 {
                preface_left: PREFACE_BYTES,
                dependency: settings.priority_dependency,
                weight: settings.priority_weight,
            },
        )
    }

    fn new(inner: T, mode: Mode) -> Self {
        Self {
            inner,
            mode,
            pending: Vec::new(),
            out: Vec::new(),
            written: 0,
        }
    }

    fn absorb(&mut self, buf: &[u8]) {
        self.pending.extend_from_slice(buf);
        match self.mode {
            Mode::Http1 => {
                while let Some(end) = find_head_end(&self.pending) {
                    rewrite_http1_head(&self.pending[..end], &mut self.out);
                    self.pending.drain(..end);
                }
            }
            Mode::Http2 {
                ref mut preface_left,
                dependency,
                weight,
            } => {
                let take = (*preface_left).min(self.pending.len());
                self.out.extend(self.pending.drain(..take));
                *preface_left -= take;
                if *preface_left > 0 {
                    return;
                }
                let mut start = 0;
                while let Some(len) = whole_frame(&self.pending[start..]) {
                    let frame = &self.pending[start..start + len];
                    if frame[3] == FRAME_HEADERS {
                        rewrite_headers_frame(frame, dependency, weight, &mut self.out);
                    } else {
                        self.out.extend_from_slice(frame);
                    }
                    start += len;
                }
                self.pending.drain(..start);
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> ShapedIo<T> {
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.written < self.out.len() {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &self.out[self.written..]))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.written += n;
        }
        self.out.clear();
        self.written = 0;
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ShapedIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ShapedIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        this.absorb(buf);
        // 这里的 Pending 已经登记了唤醒；数据留在 out 里，调用方接着会 flush
        if let Poll::Ready(Err(error)) = this.poll_drain(cx) {
            return Poll::Ready(Err(error));
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// 一个完整帧的总长度；不够一帧时是 `None`。
fn whole_frame(data: &[u8]) -> Option<usize> {
    if data.len() < FRAME_HEADER_BYTES {
        return None;
    }
    let len = usize::from(data[0]) << 16 | usize::from(data[1]) << 8 | usize::from(data[2]); // [协议] RFC 9113、RFC 7541
    let total = FRAME_HEADER_BYTES + len;
    (data.len() >= total).then_some(total)
}

fn find_head_end(data: &[u8]) -> Option<usize> {
    data.windows(4) // [协议] RFC 9113、RFC 7541
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4) // [协议] RFC 9113、RFC 7541
}

/// HPACK 整数的长度（RFC 7541 §5.1）。
fn integer_len(block: &[u8], prefix_bits: u32) -> Option<usize> {
    let mask = (1u16 << prefix_bits) - 1;
    if u16::from(*block.first()?) & mask != mask {
        return Some(1);
    }
    let rest = block[1..].iter().position(|b| b & 0x80 == 0)?; // [协议] RFC 9113、RFC 7541
    Some(rest + 2)
}

fn integer_value(block: &[u8], prefix_bits: u32) -> Option<usize> {
    let mask = (1usize << prefix_bits) - 1;
    let first = usize::from(*block.first()?) & mask;
    if first != mask {
        return Some(first);
    }
    let mut value = mask;
    for (i, byte) in block[1..].iter().enumerate() {
        value = value.checked_add(usize::from(byte & 0x7f) << (7 * i as u32))?; // [协议] RFC 9113、RFC 7541
        // [协议] RFC 9113、RFC 7541
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// HPACK 字符串的总长度（RFC 7541 §5.2）。
fn string_len(block: &[u8]) -> Option<usize> {
    let head = integer_len(block, 7)?; // [协议] RFC 9113、RFC 7541
    let body = integer_value(block, 7)?; // [协议] RFC 9113、RFC 7541
    head.checked_add(body)
}

/// 一个字段表示的总长度（RFC 7541 §6）。
fn field_len(block: &[u8]) -> Option<usize> {
    let first = *block.first()?;
    // [协议] RFC 9113、RFC 7541
    if first & 0x80 != 0 {
        return integer_len(block, 7); // [协议] RFC 9113、RFC 7541
    }
    let prefix = if first & 0x40 != 0 { 6 } else { 4 }; // [协议] RFC 9113、RFC 7541
    let mut pos = integer_len(block, prefix)?;
    if integer_value(block, prefix)? == 0 {
        pos += string_len(block.get(pos..)?)?;
    }
    pos += string_len(block.get(pos..)?)?;
    (pos <= block.len()).then_some(pos)
}

/// h2 写的顺序是 method、scheme、authority、path；换成 method、authority、scheme、path。
/// 不是预期形状时（只有 h2 换了写法才会发生）返回 `None`，由测试看住。
fn reorder_pseudo(block: &[u8]) -> Option<Vec<u8>> {
    // 开头可能有动态表大小更新（RFC 7541 §6.3）
    let mut start = 0;
    // [协议] RFC 9113、RFC 7541
    while block.get(start)? & 0xe0 == 0x20 {
        start += integer_len(&block[start..], 5)?; // [协议] RFC 9113、RFC 7541
    }
    let method = field_len(&block[start..])?;
    let scheme_at = start + method;
    let scheme = *block.get(scheme_at)?;
    if scheme != SCHEME_HTTP && scheme != SCHEME_HTTPS {
        return None;
    }
    let authority_at = scheme_at + 1;
    let authority = field_len(block.get(authority_at..)?)?;
    let mut out = Vec::with_capacity(block.len());
    out.extend_from_slice(&block[..scheme_at]);
    out.extend_from_slice(&block[authority_at..authority_at + authority]);
    out.push(scheme);
    out.extend_from_slice(&block[authority_at + authority..]);
    Some(out)
}

fn put_frame_header(out: &mut Vec<u8>, len: usize, kind: u8, flags: u8, stream: &[u8]) {
    out.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
    out.push(kind);
    out.push(flags);
    out.extend_from_slice(stream);
}

fn rewrite_headers_frame(frame: &[u8], dependency: u32, weight: u8, out: &mut Vec<u8>) {
    let flags = frame[4];
    let stream = &frame[5..FRAME_HEADER_BYTES]; // [协议] RFC 9113、RFC 7541
    let block = &frame[FRAME_HEADER_BYTES..];
    let reordered = (flags & (FLAG_PADDED | FLAG_PRIORITY) == 0)
        .then(|| reorder_pseudo(block))
        .flatten();
    let Some(block) = reordered else {
        out.extend_from_slice(frame);
        return;
    };
    // 加了优先级可能超过一帧上限（h2 按上限切的块、或很长的 Cookie）：把多出来的放进 CONTINUATION
    let head_room = MIN_MAX_FRAME - PRIORITY_BYTES;
    let flags = flags | FLAG_PRIORITY;
    if block.len() <= head_room {
        put_frame_header(
            out,
            block.len() + PRIORITY_BYTES,
            FRAME_HEADERS,
            flags,
            stream,
        );
    } else {
        put_frame_header(
            out,
            MIN_MAX_FRAME,
            FRAME_HEADERS,
            flags & !FLAG_END_HEADERS,
            stream,
        );
    }
    out.extend_from_slice(&dependency.to_be_bytes());
    out.push(weight);
    if block.len() <= head_room {
        out.extend_from_slice(&block);
        return;
    }
    out.extend_from_slice(&block[..head_room]);
    let mut chunks = block[head_room..].chunks(MIN_MAX_FRAME).peekable();
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        let continuation_flags = if last { flags & FLAG_END_HEADERS } else { 0 };
        put_frame_header(
            out,
            chunk.len(),
            FRAME_CONTINUATION,
            continuation_flags,
            stream,
        );
        out.extend_from_slice(chunk);
    }
}

/// 实录的大小写：`sec-ch-*` 全小写，其他按 `-` 分段首字母大写。
fn write_header_name(name: &[u8], out: &mut Vec<u8>) {
    if name.starts_with(b"sec-ch-") {
        out.extend_from_slice(name);
        return;
    }
    let mut upper = true;
    for &byte in name {
        out.push(if upper {
            byte.to_ascii_uppercase()
        } else {
            byte
        });
        upper = byte == b'-';
    }
}

fn rewrite_http1_head(head: &[u8], out: &mut Vec<u8>) {
    let mut lines = head.split_inclusive(|byte| *byte == b'\n');
    if let Some(request_line) = lines.next() {
        out.extend_from_slice(request_line);
    }
    for line in lines {
        match line.iter().position(|byte| *byte == b':') {
            Some(colon) => {
                write_header_name(&line[..colon], out);
                out.extend_from_slice(&line[colon..]);
            }
            None => out.extend_from_slice(line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// h2 实际写出来的形状：`:method GET`、`:scheme https`、字面量 `:authority`、`:path /`。
    fn sample_block() -> Vec<u8> {
        let mut block = vec![0x82, SCHEME_HTTPS, 0x41, 0x03];
        block.extend_from_slice(b"a.b");
        block.push(0x84);
        block
    }

    #[test]
    fn pseudo_headers_swap() {
        let mut expected = vec![0x82, 0x41, 0x03];
        expected.extend_from_slice(b"a.b");
        expected.extend_from_slice(&[SCHEME_HTTPS, 0x84]);
        assert_eq!(reorder_pseudo(&sample_block()), Some(expected));
    }

    /// 已经在动态表里的 `:authority` 是 1 字节索引；开头可能有表大小更新。
    #[test]
    fn pseudo_headers_swap_with_table_update_and_index() {
        assert_eq!(
            reorder_pseudo(&[0x3f, 0xe1, 0x1f, 0x82, SCHEME_HTTPS, 0xbe, 0x84]),
            Some(vec![0x3f, 0xe1, 0x1f, 0x82, 0xbe, SCHEME_HTTPS, 0x84])
        );
    }

    #[test]
    fn unexpected_shape_is_left_alone() {
        assert_eq!(reorder_pseudo(&[0x82, 0x84]), None);
        assert_eq!(reorder_pseudo(&[]), None);
    }

    fn headers_frame(block: &[u8], flags: u8) -> Vec<u8> {
        let mut frame = Vec::new();
        put_frame_header(&mut frame, block.len(), FRAME_HEADERS, flags, &[0, 0, 0, 1]);
        frame.extend_from_slice(block);
        frame
    }

    #[test]
    fn headers_frame_gets_priority() {
        let mut out = Vec::new();
        rewrite_headers_frame(
            &headers_frame(&sample_block(), 0x05),
            0x8000_0000,
            255,
            &mut out,
        );
        assert_eq!(whole_frame(&out), Some(out.len()));
        assert_eq!(out[3], FRAME_HEADERS);
        assert_eq!(out[4], 0x25);
        assert_eq!(&out[9..14], &[0x80, 0, 0, 0, 255]);
        assert_eq!(out[14], 0x82);
        assert_eq!(out[15], 0x41);
    }

    /// 帧已经是最大长度时，优先级字段把最后几个字节挤进 CONTINUATION，块的内容不变。
    #[test]
    fn oversized_block_spills_into_continuation() {
        let mut block = sample_block();
        // 一个长字面量把块撑到正好一帧
        let filler = MIN_MAX_FRAME - block.len() - 4;
        block.extend_from_slice(&[0x00, 0x01, b'x']);
        block.push(0x7f);
        block.extend_from_slice(&[0xff, 0x7f][..0]);
        block.resize(block.len() + filler, b'y');
        let mut out = Vec::new();
        rewrite_headers_frame(&headers_frame(&block, 0x05), 0x8000_0000, 255, &mut out);
        let first = whole_frame(&out).unwrap();
        assert_eq!(first, FRAME_HEADER_BYTES + MIN_MAX_FRAME);
        assert_eq!(out[4] & FLAG_END_HEADERS, 0);
        let second = &out[first..];
        assert_eq!(whole_frame(second), Some(second.len()));
        assert_eq!(second[3], FRAME_CONTINUATION);
        assert_eq!(second[4], FLAG_END_HEADERS);
        let total =
            (first - FRAME_HEADER_BYTES - PRIORITY_BYTES) + (second.len() - FRAME_HEADER_BYTES);
        assert_eq!(total, block.len());
    }

    #[test]
    fn http1_case() {
        let head = b"GET /a-b HTTP/1.1\r\nhost: x\r\nsec-ch-ua: \"A\"\r\nuser-agent: u-v\r\nsec-fetch-dest: document\r\n\r\n";
        let mut out = Vec::new();
        rewrite_http1_head(head, &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "GET /a-b HTTP/1.1\r\nHost: x\r\nsec-ch-ua: \"A\"\r\nUser-Agent: u-v\r\nSec-Fetch-Dest: document\r\n\r\n"
        );
    }

    /// 写被切成任意小段时结果不变（hyper 和 h2 不保证一次写一帧）。
    #[test]
    fn absorb_handles_split_writes() {
        let settings = super::super::profile::FingerprintProfile::Chrome156.data();
        let mut wire = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
        wire.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
        wire.extend_from_slice(&headers_frame(&sample_block(), 0x05));
        let mut whole = ShapedIo::http2((), &settings.http2);
        whole.absorb(&wire);
        let mut split = ShapedIo::http2((), &settings.http2);
        for byte in &wire {
            split.absorb(&[*byte]);
        }
        assert_eq!(whole.out, split.out);
        assert!(split.pending.is_empty());
        assert_eq!(whole.out[33 + 3], FRAME_HEADERS);
    }
}

//! WARC 读取（ISO 28500，WARC/1.0 和 1.1）：逐条读出记录头和记录块。
//!
//! - 不压缩的 `.warc` 和按记录分成员的 `.warc.gz`（规范附录 D）都能读；是否 gzip 看前两个字节的魔数，
//!   不看扩展名。多个 gzip 成员连续解压，记录跨不跨成员都不影响。
//! - 记录类型照原样给出（`warcinfo`、`response`、`resource`、`request`、`metadata`、`revisit`、
//!   `conversion` 等），由调用方挑；`response` 的块是 HTTP 报文，由 `pagesift_net::http_message`
//!   解析（状态行、头部、分块传输、Content-Encoding），这里不重复实现。
//! - 记录块按 `Content-Length` 读；块之后应有的两个 CRLF 宽容处理（有的写入器只写 LF）。
//!   单条记录块超过 `max_block` 时跳过块内容（流式丢弃，不分配），记录照常返回并标 `truncated`。

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use flate2::bufread::MultiGzDecoder;

/// 一条 WARC 记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRecord {
    /// `WARC/1.0` 或 `WARC/1.1`。
    pub version: String,
    /// 记录头，按出现顺序；名字保持原样（比较时不区分大小写，用 [`ReadRecord::header`]）。
    pub headers: Vec<(String, String)>,
    pub block: Vec<u8>,
    /// 块超过读取上限、内容被丢弃。
    pub truncated: bool,
}

impl ReadRecord {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// `WARC-Type`，小写。
    pub fn record_type(&self) -> String {
        self.header("WARC-Type")
            .unwrap_or_default()
            .to_ascii_lowercase()
    }

    /// `WARC-Target-URI`（去掉 WARC/1.0 允许的尖括号）。
    pub fn target_uri(&self) -> Option<&str> {
        self.header("WARC-Target-URI")
            .map(|u| u.trim().trim_start_matches('<').trim_end_matches('>'))
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("Content-Type")
    }
}

/// 逐条读记录的迭代器。
pub struct WarcReader<R: BufRead> {
    inner: R,
    max_block: u64,
    done: bool,
}

/// 打开 `.warc` 或 `.warc.gz`（按魔数判断是否 gzip）。
pub fn open(path: &Path, max_block: u64) -> io::Result<WarcReader<Box<dyn BufRead + Send>>> {
    let mut file = BufReader::new(File::open(path)?);
    // [协议] gzip 成员的魔数 1f 8b（RFC 1952 §2.3.1）
    let gz = file.fill_buf()?.starts_with(&[0x1f, 0x8b]);
    let inner: Box<dyn BufRead + Send> = if gz {
        Box::new(BufReader::new(MultiGzDecoder::new(file)))
    } else {
        Box::new(file)
    };
    Ok(WarcReader::new(inner, max_block))
}

impl<R: BufRead> WarcReader<R> {
    pub fn new(inner: R, max_block: u64) -> Self {
        Self {
            inner,
            max_block,
            done: false,
        }
    }

    fn read_line(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        buf.clear();
        self.inner.read_until(b'\n', buf)
    }

    fn next_record(&mut self) -> io::Result<Option<ReadRecord>> {
        let mut line = Vec::new();
        // 跳过记录之间多余的空行，找版本行
        let version = loop {
            if self.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            let t = trim_eol(&line);
            if t.is_empty() {
                continue;
            }
            let v = String::from_utf8_lossy(t).into_owned();
            if !v.starts_with("WARC/") {
                return Err(invalid(format!(
                    "expected a WARC version line, got {:?}",
                    truncate(&v)
                )));
            }
            break v;
        };
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            if self.read_line(&mut line)? == 0 {
                return Err(invalid("unexpected end of file in WARC headers"));
            }
            let t = trim_eol(&line);
            if t.is_empty() {
                break;
            }
            let text = String::from_utf8_lossy(t);
            // 折行（以空白开头）接到上一个头的值后面
            if text.starts_with([' ', '\t']) {
                if let Some(last) = headers.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(text.trim());
                }
                continue;
            }
            let Some((k, v)) = text.split_once(':') else {
                return Err(invalid(format!(
                    "malformed WARC header {:?}",
                    truncate(&text)
                )));
            };
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        let len: u64 = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
            .and_then(|(_, v)| v.parse().ok())
            .ok_or_else(|| invalid("WARC record without a valid Content-Length"))?;
        let (block, truncated) = if len > self.max_block {
            io::copy(&mut (&mut self.inner).take(len), &mut io::sink())?;
            (Vec::new(), true)
        } else {
            let mut block = Vec::with_capacity(len as usize);
            (&mut self.inner).take(len).read_to_end(&mut block)?;
            if (block.len() as u64) < len {
                return Err(invalid(
                    "WARC record block is shorter than its Content-Length",
                ));
            }
            (block, false)
        };
        Ok(Some(ReadRecord {
            version,
            headers,
            block,
            truncated,
        }))
    }
}

impl<R: BufRead> Iterator for WarcReader<R> {
    type Item = io::Result<ReadRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                // 结构坏了就停：后面的字节无法可靠地重新对齐到记录边界
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let mut l = line;
    while let [rest @ .., b'\n' | b'\r'] = l {
        l = rest;
    }
    l
}

fn truncate(s: &str) -> String {
    // [策略] 错误信息里引用坏行的前 80 个字符（一行终端宽度），只影响可读性
    s.chars().take(80).collect()
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::warc::{RecordType, WarcRecord, WarcWriter};

    fn sample(gzip: bool) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = WarcWriter::new(&mut buf, gzip);
            w.write_warcinfo("t.warc", "pagesift test").unwrap();
            let r = WarcRecord::new(RecordType::Resource, "text/html", b"<h1>Hi</h1>".to_vec())
                .with_target_uri("https://example.com/");
            w.write_record(&r).unwrap();
            let r = WarcRecord::new(
                RecordType::Response,
                "application/http; msgtype=response",
                b"<p>x</p>".to_vec(),
            )
            .with_target_uri("https://example.com/r")
            .with_http_response(200, vec![("Content-Type".into(), "text/html".into())]);
            w.write_record(&r).unwrap();
        }
        buf
    }

    #[test]
    fn reads_plain_and_gzip_members() {
        for gz in [false, true] {
            let bytes = sample(gz);
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("a.warc");
            std::fs::write(&p, &bytes).unwrap();
            let recs: Vec<ReadRecord> = open(&p, u64::MAX).unwrap().map(Result::unwrap).collect();
            let types: Vec<String> = recs.iter().map(ReadRecord::record_type).collect();
            assert_eq!(types, ["warcinfo", "resource", "response"], "gzip={gz}");
            assert_eq!(recs[1].block, b"<h1>Hi</h1>");
            assert_eq!(recs[1].target_uri(), Some("https://example.com/"));
            assert!(recs[2].block.starts_with(b"HTTP/1.1 200"));
        }
    }

    /// warcio 写的样本（`benchmarks/history/0.94/make_warcio_fixture.py`）：1.0 gzip 和 1.1 不压缩。
    #[test]
    fn reads_warcio_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        for (name, version) in [
            ("warcio-1.0.warc.gz", "WARC/1.0"),
            ("warcio-1.1.warc", "WARC/1.1"),
        ] {
            let recs: Vec<ReadRecord> = open(&dir.join(name), u64::MAX)
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let types: Vec<String> = recs.iter().map(ReadRecord::record_type).collect();
            assert_eq!(
                types,
                [
                    "warcinfo", "response", "request", "resource", "metadata", "response",
                    "revisit"
                ],
                "{name}"
            );
            assert!(recs.iter().all(|r| r.version == version));
            assert_eq!(recs[3].target_uri(), Some("https://fixture.test/two"));
            assert!(recs[1].block.starts_with(b"HTTP/1.1 200 OK"));
        }
    }

    #[test]
    fn warc_1_0_folding_lf_only_and_oversized_blocks() {
        let text = b"WARC/1.0\nWARC-Type: metadata\nWARC-Target-URI: <http://a.test/>\nX-Long: one\n two\nContent-Length: 5\n\nhello\n\nWARC/1.0\r\nWARC-Type: resource\r\nContent-Length: 10\r\n\r\n0123456789\r\n\r\n";
        let recs: Vec<ReadRecord> = WarcReader::new(&text[..], 8).map(Result::unwrap).collect();
        assert_eq!(recs[0].target_uri(), Some("http://a.test/"));
        assert_eq!(recs[0].header("x-long"), Some("one two"));
        assert_eq!(recs[0].block, b"hello");
        assert!(recs[1].truncated && recs[1].block.is_empty());
        let bad = b"HTTP/1.1 200 OK\r\n\r\n";
        assert!(WarcReader::new(&bad[..], 8).next().unwrap().is_err());
    }
}

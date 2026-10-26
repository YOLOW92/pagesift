//! Pure Safe Rust ISO 28500:2017 WARC/1.1 Writer.
//! Generates standard compliant multi-member Gzip WARC files with dual cryptographic digests.

use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::{self, Write};
use uuid::Uuid;

use super::checksum::DigestCalculator;

/// Standard WARC record types per ISO 28500 §5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordType {
    Warcinfo,
    Response,
    Resource,
    Request,
    Metadata,
    Revisit,
    Conversion,
    Continuation,
}

impl RecordType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warcinfo => "warcinfo",
            Self::Response => "response",
            Self::Resource => "resource",
            Self::Request => "request",
            Self::Metadata => "metadata",
            Self::Revisit => "revisit",
            Self::Conversion => "conversion",
            Self::Continuation => "continuation",
        }
    }
}

/// In-memory or streaming WARC record definition.
#[derive(Clone, Debug)]
pub struct WarcRecord {
    pub record_type: RecordType,
    pub record_id: String, // urn:uuid:<uuid>
    pub target_uri: Option<String>,
    pub date: String, // ISO-8601 UTC: YYYY-MM-DDTHH:MM:SSZ
    pub content_type: String,
    pub concurrent_to: Vec<String>,
    pub warcinfo_id: Option<String>,
    pub http_headers: Option<Vec<(String, String)>>,
    pub status_code: Option<u16>,
    pub payload: Vec<u8>,
}

impl WarcRecord {
    pub fn new(record_type: RecordType, content_type: impl Into<String>, payload: Vec<u8>) -> Self {
        let record_id = format!("urn:uuid:{}", Uuid::new_v4());
        let now = chrono_now_iso8601();
        Self {
            record_type,
            record_id,
            target_uri: None,
            date: now,
            content_type: content_type.into(),
            concurrent_to: Vec::new(),
            warcinfo_id: None,
            http_headers: None,
            status_code: None,
            payload,
        }
    }

    pub fn with_target_uri(mut self, uri: impl Into<String>) -> Self {
        self.target_uri = Some(uri.into());
        self
    }

    pub fn with_http_response(mut self, status: u16, headers: Vec<(String, String)>) -> Self {
        self.status_code = Some(status);
        self.http_headers = Some(headers);
        self
    }

    pub fn add_concurrent_to(&mut self, urn: impl Into<String>) {
        self.concurrent_to.push(urn.into());
    }
}

/// Pure Safe Rust ISO 28500 WARC/1.1 writer with optional multi-member Gzip compression.
pub struct WarcWriter<W: Write> {
    writer: W,
    gzip: bool,
    compression: Compression,
    warcinfo_id: Option<String>,
}

impl<W: Write> WarcWriter<W> {
    pub fn new(writer: W, gzip: bool) -> Self {
        Self {
            writer,
            gzip,
            compression: Compression::fast(),
            warcinfo_id: None,
        }
    }

    pub fn with_compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// Write standard warcinfo record at the beginning of the archive.
    pub fn write_warcinfo(&mut self, filename: &str, software: &str) -> io::Result<String> {
        let payload = format!(
            "software: {}\r\nformat: WARC File Format 1.1\r\nconformsTo: http://bibnum.bnf.fr/WARC/WARC_Specification_ISO_28500_version_1.1.pdf\r\nfilename: {}\r\n",
            software, filename
        );
        let record = WarcRecord::new(
            RecordType::Warcinfo,
            "application/warc-fields",
            payload.into_bytes(),
        );
        let record_id = record.record_id.clone();
        self.warcinfo_id = Some(record_id.clone());
        self.write_record(&record)?;
        Ok(record_id)
    }

    /// Serialize and write a WARC record.
    pub fn write_record(&mut self, record: &WarcRecord) -> io::Result<()> {
        // Construct the record block (HTTP headers + payload if HTTP response/request)
        let block = construct_record_block(record);
        let block_digest = DigestCalculator::digest_bytes(&block);
        let payload_digest = DigestCalculator::digest_bytes(&record.payload);

        // Build WARC header fields
        let mut warc_headers = Vec::new();
        warc_headers.push(format!("WARC-Type: {}", record.record_type.as_str()));
        warc_headers.push(format!("WARC-Record-ID: <{}>", record.record_id));
        warc_headers.push(format!("WARC-Date: {}", record.date));
        warc_headers.push(format!("Content-Length: {}", block.len()));
        warc_headers.push(format!("Content-Type: {}", record.content_type));

        if let Some(ref uri) = record.target_uri {
            warc_headers.push(format!("WARC-Target-URI: {}", uri));
        }
        if let Some(ref winfo) = record.warcinfo_id.as_ref().or(self.warcinfo_id.as_ref()) {
            warc_headers.push(format!("WARC-Warcinfo-ID: <{}>", winfo));
        }
        for concurrent in &record.concurrent_to {
            warc_headers.push(format!("WARC-Concurrent-To: <{}>", concurrent));
        }

        warc_headers.push(format!(
            "WARC-Block-Digest: {}",
            block_digest.to_warc_header()
        ));
        if record.record_type == RecordType::Response || record.record_type == RecordType::Request {
            warc_headers.push(format!(
                "WARC-Payload-Digest: {}",
                payload_digest.to_warc_header()
            ));
        }

        // Assemble uncompressed record bytes:
        // "WARC/1.1\r\n" + headers + "\r\n" + block + "\r\n\r\n"
        let mut raw_record = Vec::with_capacity(512 + block.len());
        raw_record.extend_from_slice(b"WARC/1.1\r\n");
        for h in warc_headers {
            raw_record.extend_from_slice(h.as_bytes());
            raw_record.extend_from_slice(b"\r\n");
        }
        raw_record.extend_from_slice(b"\r\n");
        raw_record.extend_from_slice(&block);
        raw_record.extend_from_slice(b"\r\n\r\n");

        if self.gzip {
            // ISO 28500 requires each record to be an independent Gzip member
            let mut encoder = GzEncoder::new(Vec::new(), self.compression);
            encoder.write_all(&raw_record)?;
            let compressed = encoder.finish()?;
            self.writer.write_all(&compressed)?;
        } else {
            self.writer.write_all(&raw_record)?;
        }

        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

fn construct_record_block(record: &WarcRecord) -> Vec<u8> {
    if record.record_type == RecordType::Response
        && let Some(status) = record.status_code
    {
        let reason = match status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            301 => "Moved Permanently",
            302 => "Found",
            304 => "Not Modified",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "Status",
        };

        let mut block = Vec::with_capacity(256 + record.payload.len());
        let status_line = format!("HTTP/1.1 {} {}\r\n", status, reason);
        block.extend_from_slice(status_line.as_bytes());

        if let Some(ref headers) = record.http_headers {
            for (k, v) in headers {
                block.extend_from_slice(format!("{}: {}\r\n", k, v).as_bytes());
            }
        }
        block.extend_from_slice(b"\r\n");
        block.extend_from_slice(&record.payload);
        block
    } else {
        record.payload.clone()
    }
}

fn chrono_now_iso8601() -> String {
    // Generate ISO-8601 UTC timestamp: YYYY-MM-DDTHH:MM:SSZ using std time
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Fast calendar conversion
    let days = secs / 86400;
    let rem_secs = secs % 86400;
    let hours = rem_secs / 3600;
    let minutes = (rem_secs % 3600) / 60;
    let seconds = rem_secs % 60;

    // Epoch day 0 is 1970-01-01
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_ymd(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

"""Write the warcio-generated WARC fixtures used by pagesift-store's reader test.

Synthetic content only. Covers WARC/1.0 and 1.1, gzip members, response (chunked + gzip-encoded
body), resource, request, metadata and revisit records.

    python -I make_warcio_fixture.py crates/pagesift-store/tests/data
"""

import gzip
import io
import pathlib
import sys

from warcio.statusandheaders import StatusAndHeaders
from warcio.warcwriter import WARCWriter


def chunked(body: bytes, size: int) -> bytes:
    out = b""
    for i in range(0, len(body), size):
        part = body[i:i + size]
        out += b"%x\r\n" % len(part) + part + b"\r\n"
    return out + b"0\r\n\r\n"


def write(path: pathlib.Path, version: str, gz: bool):
    with open(path, "wb") as f:
        w = WARCWriter(f, gzip=gz, warc_version=version)
        w.write_record(w.create_warcinfo_record(path.name, {"software": "warcio fixture"}))
        html = b"<html><head><title>Fixture one</title></head><body><h1>Fixture one</h1><p>Archived with warcio, gzip and chunked.</p></body></html>"
        payload = chunked(gzip.compress(html), 37)
        http = StatusAndHeaders("200 OK", [("Content-Type", "text/html; charset=utf-8"), ("Content-Encoding", "gzip"), ("Transfer-Encoding", "chunked")], protocol="HTTP/1.1")
        resp = w.create_warc_record("https://fixture.test/one", "response", payload=io.BytesIO(payload), http_headers=http)
        req_http = StatusAndHeaders("GET /one HTTP/1.1", [("Host", "fixture.test")], is_http_request=True)
        req = w.create_warc_record("https://fixture.test/one", "request", payload=io.BytesIO(b""), http_headers=req_http)
        w.write_request_response_pair(req, resp)
        res = w.create_warc_record("https://fixture.test/two", "resource", payload=io.BytesIO(b"<html><body><h1>Fixture two</h1><p>A resource record.</p></body></html>"), warc_content_type="text/html")
        w.write_record(res)
        meta = w.create_warc_record("https://fixture.test/two", "metadata", payload=io.BytesIO(b'{"k": "v"}'), warc_content_type="application/json")
        w.write_record(meta)
        notfound = StatusAndHeaders("404 Not Found", [("Content-Type", "text/html")], protocol="HTTP/1.1")
        w.write_record(w.create_warc_record("https://fixture.test/missing", "response", payload=io.BytesIO(b"<p>gone</p>"), http_headers=notfound))
        revisit = w.create_revisit_record("https://fixture.test/one", digest="sha1:AAAA", refers_to_uri="https://fixture.test/one", refers_to_date="2026-01-01T00:00:00Z")
        w.write_record(revisit)


if __name__ == "__main__":
    out = pathlib.Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    write(out / "warcio-1.0.warc.gz", "1.0", True)
    write(out / "warcio-1.1.warc", "1.1", False)

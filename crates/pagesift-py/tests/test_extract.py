"""store_path：选择器记忆和重定位。"""

import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

import pagesift

SCHEMA = {
    "name": "t",
    "baseSelector": "main",
    "fields": [{"name": "title", "selector": ".title-v1", "type": "text"}],
}
ROWS = "".join(f"<section><h2>Part {i}</h2><p>Paragraph {i} in words.</p></section>" for i in range(8))


def test_extract_relocates_with_store_when_max_age_is_zero(tmp_path):
    state = {"cls": "title-v1"}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            body = f'<html><body><main><h1 class="{state["cls"]}">Hello Widget</h1>{ROWS}</main></body></html>'
            self.send_response(200)
            self.send_header("content-type", "text/html")
            self.end_headers()
            self.wfile.write(body.encode())

        def log_message(self, *args):
            pass

    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{srv.server_port}/"
    try:
        s = pagesift.Scraper(allow_private_network=True, request_interval_ms=0, store_path=str(tmp_path / "store"))
        s.extract([url], SCHEMA, max_age=0)
        state["cls"] = "title-v2"
        d = s.extract([url], SCHEMA, max_age=0)[0]
        assert d.extract == [{"title": "Hello Widget"}]
        assert any(w.startswith("relocated: fields.title") for w in d.warnings)
    finally:
        srv.shutdown()


def test_ocr_options_are_checked():
    with pytest.raises(ValueError, match="need OCR turned on"):
        pagesift.Scraper(ocr_quality="fast")
    with pytest.raises(ValueError, match="OCR quality must be"):
        pagesift.Scraper(ocr=True, ocr_quality="best")
    with pytest.raises(ValueError, match="gpu mode must be"):
        pagesift.Scraper(ocr=True, gpu="metal")
    with pytest.raises(pagesift.InvalidRequest):
        pagesift.ScrapeOptions(parsers=[{"type": "pdf", "mode": "best"}])

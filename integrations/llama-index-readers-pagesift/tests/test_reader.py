"""不访问外网：网页来自本机服务器，文件来自临时目录。"""

from __future__ import annotations

import asyncio
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pagesift
import pytest
from llama_index.core.schema import Document
from llama_index.readers.pagesift import PagesiftReader

BODY = "".join(f"<p>Paragraph {i} with enough words to read like normal prose for people.</p>" for i in range(30))


def page(title: str, links: str = "") -> bytes:
    return f"<!DOCTYPE html><html lang='en'><head><title>{title}</title></head><body><article><h1>{title}</h1>{links}{BODY}</article></body></html>".encode()


PAGES = {
    "/one": page("One"),
    "/site/": page("Home", "<p><a href='/site/a'>a</a></p>"),
    "/site/a": page("Leaf"),
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        body = PAGES.get(self.path)
        self.send_response(200 if body else 404)
        self.send_header("Content-Type", "text/html; charset=utf-8" if body else "text/plain")
        self.end_headers()
        self.wfile.write(body or b"missing")

    def log_message(self, *_args) -> None:
        pass


@pytest.fixture(scope="module")
def base():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{server.server_port}"
    server.shutdown()


@pytest.fixture(scope="module")
def scraper():
    with pagesift.Scraper(allow_private_network=True, request_interval_ms=0) as s:
        yield s


def test_urls_become_documents(base, scraper):
    docs = PagesiftReader(scraper=scraper).load_data(urls=f"{base}/one")
    assert len(docs) == 1
    assert isinstance(docs[0], Document)
    assert "Paragraph 1" in docs[0].text
    assert docs[0].metadata["source"] == f"{base}/one"
    assert docs[0].metadata["title"] == "One"
    assert "status_code" in docs[0].excluded_embed_metadata_keys
    assert "status_code" in docs[0].excluded_llm_metadata_keys


def test_crawl_follows_links_within_the_limit(base, scraper):
    reader = PagesiftReader(scraper=scraper)
    docs = reader.load_data(crawl=f"{base}/site/", limit=5, sitemap="skip")
    assert {d.metadata["title"] for d in docs} == {"Home", "Leaf"}
    assert len(reader.load_data(crawl=f"{base}/site/", limit=1)) == 1


def test_files_are_read_by_content_and_name(tmp_path, scraper):
    note = tmp_path / "note.md"
    note.write_text("# Note\n\nbody\n", encoding="utf-8")
    mail = tmp_path / "m.eml"
    mail.write_bytes(b"Subject: Hello\r\nFrom: a@example.com\r\n\r\nmail body\r\n")
    docs = PagesiftReader(scraper=scraper).load_data(files=[note, str(mail)])
    by_source = {d.metadata["source"]: d for d in docs}
    assert by_source["note.md"].text.strip().endswith("body")
    assert by_source["note.md"].metadata["title"] == "Note"
    assert "mail body" in by_source["m.eml"].text


def test_failures_are_skipped_or_raised(scraper, tmp_path):
    blob = tmp_path / "x.bin"
    blob.write_bytes(b"\0\1\2")
    assert PagesiftReader(scraper=scraper).load_data(files=blob) == []
    with pytest.raises(pagesift.PagesiftError):
        PagesiftReader(scraper=scraper, raise_on_error=True).load_data(files=blob)


def test_async_and_lazy_paths_match(base, scraper):
    reader = PagesiftReader(scraper=scraper)
    url = f"{base}/one"
    assert [d.metadata["title"] for d in asyncio.run(reader.aload_data(urls=[url]))] == ["One"]
    assert [d.metadata["title"] for d in reader.lazy_load_data(urls=[url])] == ["One"]


def test_exactly_one_source_is_required():
    reader = PagesiftReader()
    for kwargs in ({}, {"urls": ["u"], "crawl": "c"}, {"urls": ["u"], "files": ["f"]}):
        with pytest.raises(ValueError):
            reader.load_data(**kwargs)


def test_owned_scraper_is_created_and_closed(tmp_path):
    note = tmp_path / "a.txt"
    note.write_text("plain words", encoding="utf-8")
    docs = PagesiftReader().load_data(files=note)
    assert docs[0].text.strip() == "plain words"

"""本地快照回放服务：抓取类工具（spider、Scrapy、Crawl4AI）都从这里读同一份快照，不各自联网。

    python3 -I bench/mix/replay.py <快照根目录> <端口>

只监听 127.0.0.1。<路径>.headers.json 存在时按里面记录的状态码和响应头回放（抽样抓快照时写，
格式 {"status": 200, "headers": [["Content-Type", "text/html; charset=..."], ...]}）；
没有就按 200、Content-Type: text/html（不带 charset，各工具自己识别编码）回放。robots.txt 不存在时回 404。
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(sys.argv[1]).resolve()
SKIP = {"content-length", "transfer-encoding", "content-encoding", "connection"}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_HEAD(self):
        self.do_GET(head=True)

    def do_GET(self, head=False):
        rel = unquote(urlsplit(self.path).path).lstrip("/")
        p = (ROOT / rel).resolve()
        if ROOT not in p.parents and p != ROOT or not p.is_file():
            body = b"not found"
            self.send_response(404)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if not head:
                self.wfile.write(body)
            return
        body = p.read_bytes()
        meta = p.with_name(p.name + ".headers.json")
        status, headers = 200, [["Content-Type", "text/html"]]
        if meta.is_file():
            m = json.loads(meta.read_text())
            status, headers = m.get("status", 200), m.get("headers", headers)
        self.send_response(status)
        for k, v in headers:
            if k.lower() not in SKIP:
                self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if not head:
            self.wfile.write(body)

    def log_message(self, *a):
        pass


ThreadingHTTPServer.daemon_threads = True
ThreadingHTTPServer(("127.0.0.1", int(sys.argv[2])), H).serve_forever()

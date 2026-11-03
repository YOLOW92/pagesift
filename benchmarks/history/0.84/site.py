#!/usr/bin/env python3
"""0.84 逐字比对用的本地站点：按清单逐页提供 HTML。

  site.py <manifest.jsonl> <port>

`/p/<序号>` 返回第 <序号> 页的原始字节，不带 charset（让抓取方自己判断编码）；
序号除以 3 余 1 的用 gzip、余 2 的用 deflate（zlib 格式）压缩，覆盖解压路径。
另有 `/feed.xml`（RSS）和 `/data.json`（JSON）各一页。
"""
import gzip
import json
import sys
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGES = [json.loads(l)["path"] for l in open(sys.argv[1], encoding="utf-8") if l.strip()]
FEED = b"""<?xml version="1.0"?><rss version="2.0"><channel><title>Feed</title>
<item><title>First</title><link>http://example.com/1</link><description>One</description></item>
<item><title>Second</title><link>http://example.com/2</link><description>Two</description></item>
</channel></rss>"""
JSON = b'{"title": "Data", "items": [1, 2, 3], "text": "plain json body"}'


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, body, ctype, encoding=None):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        if encoding:
            self.send_header("Content-Encoding", encoding)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/feed.xml":
            return self.send(FEED, "application/rss+xml")
        if self.path == "/data.json":
            return self.send(JSON, "application/json")
        if not self.path.startswith("/p/"):
            self.send_response(404)
            self.end_headers()
            return
        i = int(self.path[3:])
        body = open(PAGES[i], "rb").read()
        if i % 3 == 1:
            return self.send(gzip.compress(body, mtime=0), "text/html", "gzip")
        if i % 3 == 2:
            return self.send(zlib.compress(body), "text/html", "deflate")
        return self.send(body, "text/html")


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[2])), Handler).serve_forever()

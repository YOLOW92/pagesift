#!/usr/bin/env python3
"""E1-B/C 受控源站：确定性页面图 + 可复现故障注入。

用法: e1_origin.py <port> <host-id> [fault_density] [seed]
路由：
  /robots.txt            每主机不同策略（含 Crawl-delay）
  /sitemap.xml           列出部分 /a/ 页
  /list?page=N           分页列表，链接到条目与下一页（有环：末页回到首页）
  /a/<id>                文章页：正文 + 相关链接（互链成环）+ ?ref= 变体 + canonical
  /a/<id>?ref=X          与 /a/<id> 同内容（参数去重用例）
  /canon/<id>            带 <link rel=canonical> 指向 /a/<id>
  /deep/<n>              深链（n 递减）
  /r/<n>                 302 链
  /r/loop                重定向环
  /pdf/<n>               真实 PDF（来自 olmOCR 样本）
  /js/<n>                正文只在脚本执行后出现（SPA）
  /fault/<kind>          显式故障：429 503 timeout slow truncated malformed garbage
  /go-private            302 到私网地址（SSRF 越界用例）

确定性：同一 URL 的内容固定；每页嵌入唯一标记 <URL-MARKER:...> 用于唯一内容核验。
故障注入：fault_density>0 时，对普通页面按 seed+URL 哈希决定是否注入故障，
  保证同一 seed 下每次运行得到完全相同的故障集合。
"""
import hashlib, os, sys, time, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8110
HOSTID = sys.argv[2] if len(sys.argv) > 2 else "h1"
DENSITY = float(sys.argv[3]) if len(sys.argv) > 3 else 0.0
SEED = sys.argv[4] if len(sys.argv) > 4 else "e1-2026"
CRAWL_DELAY = int(sys.argv[5]) if len(sys.argv) > 5 else 0
PDF_DIR = "/home/yolow92/pagesift/bench/data/olmocr/bench/bench_data/pdfs"

ARTICLES = 60
LIST_PAGES = 8
DEEP = 6
PDFS = []

def markers(url):
    return f"<URL-MARKER:{url}>"

def article_body(i, url):
    para = (f"Article {i} on {HOSTID}. This paragraph exists so the extractor has real prose to "
            f"select. It repeats with variation {i} so the content differs per URL. ")
    return para * 4 + markers(url)

def page(url, title, body, links=(), canonical=None, js_content=None):
    ls = "".join(f'<li><a href="{h}">{t}</a></li>' for h, t in links)
    canon = f'<link rel="canonical" href="{canonical}">' if canonical else ""
    js = ""
    if js_content is not None:
        js = f"""<div id="app"></div><script>
document.getElementById('app').innerHTML = {js_content!r};
</script>"""
        body = ""
    return f"""<!doctype html><html><head><meta charset="utf-8"><title>{title}</title>{canon}</head>
<body><h1>{title}</h1><main><p>{body}</p><ul>{ls}</ul></main>{js}</body></html>"""

def fault_for(url):
    """按 seed+URL 决定故障类型；返回 None 表示正常。"""
    if DENSITY <= 0:
        return None
    hv = int(hashlib.sha256((SEED + url).encode()).hexdigest()[:8], 16)
    if (hv % 1000) / 1000.0 >= DENSITY:
        return None
    kinds = ["429", "503", "slow", "truncated", "malformed", "timeout"]
    return kinds[hv % len(kinds)]

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a):
        pass

    def _send(self, code, body=b"", ctype="text/html; charset=utf-8", headers=None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET(head=True)

    def do_GET(self, head=False):
        u = urlparse(self.path)
        path, q = u.path, parse_qs(u.query)
        full = f"{HOSTID}{self.path}"

        # 显式故障端点
        if path.startswith("/fault/"):
            kind = path.split("/")[-1]
            return self._inject(kind, full)

        # 私网重定向（SSRF 越界）
        if path == "/go-private":
            return self._send(302, b"", headers={"Location": "http://10.255.255.1/secret"})

        # 按 seed 注入
        kind = fault_for(full)
        if kind:
            return self._inject(kind, full)

        if path == "/robots.txt":
            rules = [f"User-agent: *", "Disallow: /private/", "Allow: /"]
            if CRAWL_DELAY > 0:
                rules.append(f"Crawl-delay: {CRAWL_DELAY}")
            if HOSTID == "h3":
                rules = ["User-agent: *", "Disallow: /"]
            return self._send(200, ("\n".join(rules) + "\n").encode(), "text/plain; charset=utf-8")

        if path == "/sitemap.xml":
            items = "".join(
                f"<url><loc>http://127.0.0.1:{PORT}/a/sm{i}</loc></url>" for i in range(0, ARTICLES, 7))
            body = f'<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{items}</urlset>'
            return self._send(200, body.encode(), "application/xml")

        if path == "/list":
            n = int(q.get("page", ["0"])[0])
            links = [(f"/a/{n*8+i}", f"Item {n*8+i}") for i in range(8)]
            links.append((f"/list?page={(n+1) % LIST_PAGES}", "Next"))
            links.append(("/list?page=0", "Home"))
            return self._send(200, page(full, f"List {n}", f"Listing page {n} for {HOSTID}. " * 6,
                                        links).encode())

        if path.startswith("/a/"):
            ident = path[3:]
            body = article_body(ident, full)
            links = [
                (f"/a/{(hashlib.sha256(ident.encode()).digest()[0]) % ARTICLES}", "Related"),
                (f"/a/{ident}?ref=side", "Same item via param"),
                (f"/canon/{ident}", "Canonical twin"),
                (f"/list?page=0", "Back to list"),
                (f"/pdf/{ident}", "PDF version"),
            ]
            return self._send(200, page(full, f"Article {ident}", body, links).encode())

        if path.startswith("/canon/"):
            ident = path[7:]
            return self._send(200, page(full, f"Canon {ident}", article_body(ident, full),
                                        [(f"/a/{ident}", "Original")],
                                        canonical=f"http://127.0.0.1:{PORT}/a/{ident}").encode())

        if path.startswith("/deep/"):
            n = int(path.split("/")[-1] or 0)
            links = [(f"/deep/{n-1}", f"Deeper {n-1}")] if n > 0 else []
            return self._send(200, page(full, f"Deep {n}", f"Deep page level {n}. " * 5, links).encode())

        if path.startswith("/r/loop"):
            return self._send(302, b"", headers={"Location": "/r/loop"})

        if path.startswith("/r/"):
            n = int(path.split("/")[-1] or 0)
            dest = f"/r/{n-1}" if n > 0 else "/a/r0"
            return self._send(302, b"", headers={"Location": dest})

        if path.startswith("/pdf/"):
            if not PDFS:
                return self._send(404, b"no pdf")
            p = PDFS[int(hashlib.sha256(path.encode()).hexdigest()[:6], 16) % len(PDFS)]
            with open(p, "rb") as f:
                data = f.read()
            return self._send(200, data, "application/pdf")

        if path.startswith("/js/"):
            n = path.split("/")[-1]
            inner = f"<p>{article_body(n, full)}</p>"
            return self._send(200, page(full, f"JS {n}", "", js_content=inner).encode())

        if path == "/":
            links = [(f"/list?page={i}", f"List {i}") for i in range(LIST_PAGES)]
            links += [(f"/a/root{i}", f"Root article {i}") for i in range(5)]
            return self._send(200, page(full, f"{HOSTID} index",
                                        f"Index of {HOSTID}. " * 6, links).encode())

        return self._send(404, b"<h1>404</h1>")

    def _inject(self, kind, full):
        if kind == "429":
            return self._send(429, b"rate limited", headers={"Retry-After": "1"})
        if kind == "503":
            return self._send(503, b"unavailable")
        if kind == "slow":
            time.sleep(2.5)
            return self._send(200, page(full, "Slow", "Slow page. " * 10).encode())
        if kind == "timeout":
            time.sleep(30)
            return self._send(200, b"late")
        if kind == "truncated":
            body = page(full, "Truncated", "Truncated body. " * 20).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body[: len(body) // 3])   # 声明长度大于实际写出
            return
        if kind == "malformed":
            return self._send(200, b"<html><body><p>unclosed <div><table><tr><td>x</td>", "text/html")
        return self._send(200, b"<html><body>ok</body></html>")

def main():
    global PDFS
    if os.path.isdir(PDF_DIR):
        for root, _, files in os.walk(PDF_DIR):
            for f in files:
                if f.endswith(".pdf"):
                    PDFS.append(os.path.join(root, f))
        PDFS.sort()
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), H)
    srv.daemon_threads = True
    print(f"{HOSTID} on :{PORT} density={DENSITY} seed={SEED} pdfs={len(PDFS)}", flush=True)
    srv.serve_forever()

if __name__ == "__main__":
    main()

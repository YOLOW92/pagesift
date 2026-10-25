"""本地测试服务器：不访问外网。"""

from __future__ import annotations

import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

import pytest

import pagesift

PROSE = "".join(
    f"<p>Paragraph {i} of a normal article, with enough words to read like real prose "
    "that someone wrote for people rather than for robots or crawlers.</p>"
    for i in range(30)
)


def article(title: str, extra: str = "") -> str:
    return f"""<!DOCTYPE html><html lang="en"><head><title>{title}</title>
<meta name="description" content="About {title}">
<meta property="og:image" content="/og.png">
</head><body>
<nav><a href="/">Home</a> <a href="/about">About</a></nav>
<article><h1>{title}</h1>{extra}{PROSE}
<p>See <a href="/next">the next page</a>.</p><img src="/pic.png" alt="pic"></article>
<footer>Copyright footer text</footer></body></html>"""


CHALLENGE = """<!DOCTYPE html><html><head><title>Just a moment...</title></head>
<body><div class="main-wrapper"><h1>example.com</h1>
<h2 id="challenge-running">Checking if the site connection is secure</h2>
<noscript>Enable JavaScript and cookies to continue</noscript></div>
<script>(function(){window._cf_chl_opt={cvId:'3',cType:'managed'};
var a=document.createElement('script');a.src='/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=1';
document.head.appendChild(a);}());</script></body></html>"""


class Handler(BaseHTTPRequestHandler):
    slow_lock = threading.Lock()
    slow_active: dict[str, int] = {}
    slow_peak: dict[str, int] = {}

    def log_message(self, *args: object) -> None:
        pass

    def send(self, status: int, body: str, ctype: str = "text/html; charset=utf-8", **headers: str) -> None:
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        for k, v in headers.items():
            self.send_header(k.replace("_", "-"), v)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        u = urlparse(self.path)
        q = parse_qs(u.query)
        p = u.path
        if p == "/robots.txt":
            self.send(200, "User-agent: *\nDisallow: /private\n", "text/plain")
        elif p.startswith("/page/") or p == "/article" or p.startswith("/private"):
            self.send(200, article(f"Page {p}"))
        elif p == "/slow":
            probe = q.get("probe", [None])[0]
            if probe:
                with self.slow_lock:
                    active = Handler.slow_active.get(probe, 0) + 1
                    Handler.slow_active[probe] = active
                    Handler.slow_peak[probe] = max(Handler.slow_peak.get(probe, 0), active)
            try:
                time.sleep(int(q.get("ms", ["300"])[0]) / 1000)
                self.send(200, article(f"Slow {q.get('ms', [''])[0]}"))
            finally:
                if probe:
                    with self.slow_lock:
                        Handler.slow_active[probe] -= 1
        elif p == "/headers":
            self.send(200, article("Headers", f"<p>X-Test header is {self.headers.get('X-Test')}.</p>"))
        elif p == "/redirect":
            self.send(302, "", Location="/article")
        elif p == "/challenge":
            self.send(403, CHALLENGE)
        elif p == "/ratelimit":
            self.send(429, "slow down", "text/plain", Retry_After="30")
        elif p == "/error500":
            self.send(500, article("Server error"))
        else:
            self.send(404, article("Not found"))


class Server(ThreadingHTTPServer):
    def handle_error(self, request, client_address) -> None:
        # 取消抓取时客户端提前断开，忽略 BrokenPipe
        pass


@pytest.fixture(scope="session")
def server():
    httpd = Server(("127.0.0.1", 0), Handler)
    httpd.daemon_threads = True
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    yield f"http://127.0.0.1:{httpd.server_address[1]}"
    httpd.shutdown()


@pytest.fixture
def slow_probe():
    class Probe:
        id = uuid.uuid4().hex

        @property
        def peak(self):
            with Handler.slow_lock:
                return Handler.slow_peak.get(self.id, 0)

    yield Probe()
    with Handler.slow_lock:
        Handler.slow_active.pop(Probe.id, None)
        Handler.slow_peak.pop(Probe.id, None)


@pytest.fixture
def scraper():
    """每个测试一个新的 Scraper：冷却等主机状态互不影响。本机地址需要显式放行。"""
    with pagesift.Scraper(
        allow_private_network=True, request_interval=0, per_host_concurrency=16
    ) as s:
        yield s


@pytest.fixture
def http() -> pagesift.ScrapeOptions:
    """测试里只用 HTTP 引擎，不依赖本机有没有 Chrome。"""
    return pagesift.ScrapeOptions(engine="http")


@pytest.fixture
def closed_port() -> int:
    import socket

    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port

#!/usr/bin/env python3
"""D9 动态渲染测试床：一批确定性的 SPA / 动态内容页面。

覆盖场景（每种都要"等脚本跑完才看得到正文"）：
  /csr       纯客户端渲染：正文由 JS 注入到 #app
  /delay     延迟 1.5s 后注入
  /scroll    内容在滚动后才加载（需要滚动触发）
  /click     内容在点击按钮后才出现
  /canvas    canvas 绘制（文本在像素里，DOM 无文本）
  /spa       前端路由：/spa/a 与 /spa/b 由 JS 切换
  /static    纯静态对照组（HTTP 模式也能拿到正文）
  /hydration 服务端渲染骨架 + JS 补水后替换内容
每个页面都带唯一标记便于校验。
"""
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8200

def page(title, body, script=""):
    return f"""<!doctype html><html><head><meta charset="utf-8"><title>{title}</title></head>
<body><h1>{title}</h1>{body}{script}</body></html>"""

PAGES = {
    "/static": page("Static Page",
        "<p>Static content that any HTTP fetcher can read. "
        "This paragraph exists as a control case. " * 3 + "MARKER-STATIC</p>"),
    "/csr": page("CSR Page", '<div id="app">Loading...</div>',
        """<script>
        const d = document.createElement('div');
        d.innerHTML = '<h2>Client Rendered</h2><p>' + 'This text only exists after JavaScript runs. '.repeat(6) + 'MARKER-CSR</p>';
        document.getElementById('app').innerHTML = '';
        document.getElementById('app').appendChild(d);
        </script>"""),
    "/delay": page("Delayed Page", '<div id="app">Loading...</div>',
        """<script>
        setTimeout(() => {
          document.getElementById('app').innerHTML =
            '<h2>Delayed Render</h2><p>' + 'This content appears 1500ms after load. '.repeat(6) + 'MARKER-DELAY</p>';
        }, 1500);
        </script>"""),
    "/scroll": page("Scroll Page",
        '<div id="app"><p>' + 'Initial visible text before scrolling. ' * 4 + '</p></div>',
        """<script>
        window.addEventListener('scroll', () => {
          if (!document.getElementById('lazy')) {
            const d = document.createElement('div');
            d.id = 'lazy';
            d.innerHTML = '<p>' + 'Lazy loaded after scroll event fires. '.repeat(6) + 'MARKER-SCROLL</p>';
            document.getElementById('app').appendChild(d);
          }
        });
        // 有些页面即使不滚也会在短暂延迟后自己加进来
        setTimeout(() => window.dispatchEvent(new Event('scroll')), 800);
        </script>"""),
    "/click": page("Click Page",
        '<div id="app"><button id="go">Load</button></div>',
        """<script>
        document.getElementById('go').addEventListener('click', () => {
          document.getElementById('app').innerHTML =
            '<p>' + 'Content revealed by a click on the button. '.repeat(6) + 'MARKER-CLICK</p>';
        });
        </script>"""),
    "/canvas": page("Canvas Page", '<canvas id="c" width="400" height="100"></canvas>',
        """<script>
        const ctx = document.getElementById('c').getContext('2d');
        ctx.font = '16px sans-serif';
        ctx.fillText('MARKER-CANVAS drawn on canvas', 10, 50);
        </script>"""),
    "/hydration": page("Hydration Page",
        '<div id="app"><p>Placeholder text before hydration.</p></div>',
        """<script>
        document.getElementById('app').innerHTML =
          '<h2>Hydrated</h2><p>' + 'Server sent a placeholder, JS replaced it with real content. '.repeat(6) + 'MARKER-HYDRATION</p>';
        </script>"""),
}

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self):
        p = self.path.split("?")[0]
        if p == "/robots.txt":
            body = b"User-agent: *\nAllow: /\n"
            self.send_response(200); self.send_header("Content-Type","text/plain")
            self.send_header("Content-Length",str(len(body))); self.end_headers()
            self.wfile.write(body); return
        if p == "/index":
            items = "".join(f'<li><a href="{k}">{k}</a></li>' for k in PAGES)
            body = page("Index", f"<ul>{items}</ul>").encode()
        elif p in PAGES:
            body = PAGES[p].encode()
        elif p.startswith("/spa"):
            body = page("SPA", '<div id="app"></div>',
                """<script>
                const path = location.pathname;
                document.getElementById('app').innerHTML =
                  '<p>SPA route ' + path + '. ' + 'Routes are resolved client side only. '.repeat(5) + 'MARKER-SPA-' + path + '</p>';
                </script>""").encode()
        else:
            self.send_response(404); self.send_header("Content-Length","0"); self.end_headers(); return
        self.send_response(200)
        self.send_header("Content-Type","text/html; charset=utf-8")
        self.send_header("Content-Length",str(len(body)))
        self.end_headers(); self.wfile.write(body)

if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
    srv.daemon_threads = True
    print(f"D9 origin on :{PORT}", flush=True)
    srv.serve_forever()

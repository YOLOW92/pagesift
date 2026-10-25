#!/usr/bin/env python3
"""D9 动态渲染竞品：Playwright（Python）与 puppeteer（Node，另一文件）。
对每个页面等网络空闲后取可见文本，检查唯一标记是否出现。"""
import json, sys, time
from playwright.sync_api import sync_playwright

PAGES = ["static", "csr", "delay", "scroll", "click", "canvas", "hydration"]
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8200
out = sys.argv[2] if len(sys.argv) > 2 else "/out/d9/playwright.json"

res = []
with sync_playwright() as p:
    b = p.chromium.launch(args=["--no-sandbox"])
    ctx = b.new_context()
    for name in PAGES:
        pg = ctx.new_page()
        t0 = time.perf_counter()
        try:
            pg.goto(f"http://127.0.0.1:{PORT}/{name}", wait_until="networkidle", timeout=30000)
            # 额外等一小会儿，覆盖 setTimeout 场景
            pg.wait_for_timeout(2000)
            text = pg.inner_text("body")
        except Exception as e:
            text = f"ERROR {type(e).__name__}: {e}"
        ms = (time.perf_counter() - t0) * 1000
        expect = "MARKER-" + name.upper().replace("-", "")
        res.append({"page": name, "expect": expect, "found": expect in text,
                    "chars": len(text), "ms": round(ms, 1),
                    "snippet": text[:120].replace("\n", " ")})
        pg.close()
    b.close()
json.dump(res, open(out, "w"), ensure_ascii=False, indent=2)
print(json.dumps([{"page": r["page"], "found": r["found"], "ms": r["ms"]} for r in res]))

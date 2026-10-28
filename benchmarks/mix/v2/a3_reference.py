"""第 2 版 A3 的参照文本（RULES.md 第二节）：用不参评的 Playwright Firefox 渲染每个页面，取 body.innerText。
所有工具跑之前一次、之后一次，评分只用两次都出现的词。用 Crawl4AI 环境里的 Playwright（Firefox 是另装的浏览器，
Crawl4AI 自己用 Chromium，所以参照和任何参评工具都不是同一个引擎）。

    PLAYWRIGHT_BROWSERS_PATH=$MIX/ms-playwright $MIX/py/crawl4ai/bin/python -I benchmarks/mix/v2/a3_reference.py <清单> <输出目录> [并发]

清单每行 `id<TAB>url`。每页：打开 → 等 load 事件 → 再等网络空闲（最多 15 s）→ 再等 2 s → 取 innerText，写 <输出目录>/<id>.txt；
打不开的写 <id>.err。单页上限 60 s（和 A3 的单份上限一样）。
"""
import asyncio
import sys
from pathlib import Path

from playwright.async_api import async_playwright

UA_NOTE = "pagesift-mix-bench/0.80 (+https://github.com/YOLOW92/pagesift)"


async def one(browser, id_, url, out, sem):
    async with sem:
        ctx = await browser.new_context()
        page = await ctx.new_page()
        try:
            await asyncio.wait_for(render(page, url, out / f"{id_}.txt"), timeout=60)
        except Exception as e:  # noqa: BLE001  打不开、超时都照实记
            (out / f"{id_}.err").write_text(f"{type(e).__name__}: {e}"[:500])
        finally:
            await ctx.close()


async def render(page, url, dest):
    await page.goto(url, wait_until="load", timeout=45000)
    try:
        await page.wait_for_load_state("networkidle", timeout=15000)
    except Exception:  # noqa: BLE001  一直有请求的页面不等网络空闲
        pass
    await page.wait_for_timeout(2000)
    dest.write_text(await page.inner_text("body"), encoding="utf-8")


async def main():
    manifest, out = Path(sys.argv[1]), Path(sys.argv[2])
    conc = int(sys.argv[3]) if len(sys.argv) > 3 else 4
    out.mkdir(parents=True, exist_ok=True)
    rows = [l.split("\t")[:2] for l in manifest.read_text().splitlines() if l.strip()]
    sem = asyncio.Semaphore(conc)
    async with async_playwright() as p:
        browser = await p.firefox.launch(headless=True)
        await asyncio.gather(*(one(browser, i, u, out, sem) for i, u in rows))
        await browser.close()
    ok = sum(1 for i, _ in rows if (out / f"{i}.txt").exists())
    print(f"参照渲染：{ok}/{len(rows)} 页")


if __name__ == "__main__":
    asyncio.run(main())

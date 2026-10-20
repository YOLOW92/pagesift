"""Crawl4AI 整站爬取：对每个站点做 BFS 深度爬取（同一个 AsyncWebCrawler，所有站点并发）。

python crawl4ai_crawl.py <port> <hosts> <pages> <interval-ms> <http|browser> [per-host]
礼貌规则和 pagesift 对齐：检查 robots.txt；每个域名的请求间隔固定为 interval（mean_delay，max_range=0）；
全局并发 = hosts × per-host。
"""
import asyncio, collections, sys, time
from crawl4ai import AsyncWebCrawler, BrowserConfig, CrawlerRunConfig, CacheMode
from crawl4ai.async_crawler_strategy import AsyncHTTPCrawlerStrategy
from crawl4ai.deep_crawling import BFSDeepCrawlStrategy

async def main():
    port, hosts, pages, interval = (int(x) for x in sys.argv[1:5])
    mode = sys.argv[5]
    per_host = int(sys.argv[6]) if len(sys.argv) > 6 else 2
    kwargs = {}
    raw_errors = collections.Counter()
    if mode == "http":
        strategy = AsyncHTTPCrawlerStrategy()

        async def on_error(e):
            raw_errors[f"{type(e).__name__}: {str(e)[:100]}"] += 1

        strategy.hooks["on_error"] = on_error
        kwargs["crawler_strategy"] = strategy
    # 用系统装的 Chrome（和 pagesift 同一个浏览器），不用 Playwright 另外下载的那份
    browser = BrowserConfig(headless=True, verbose=False, chrome_channel="chrome", channel="chrome")
    t = time.monotonic()
    ok = err = chars = 0
    reasons = collections.Counter()
    async with AsyncWebCrawler(config=browser, **kwargs) as crawler:
        async def site(h):
            nonlocal ok, err, chars
            cfg = CrawlerRunConfig(
                deep_crawl_strategy=BFSDeepCrawlStrategy(max_depth=100, max_pages=pages),
                cache_mode=CacheMode.BYPASS,
                check_robots_txt=True,
                mean_delay=interval / 1000, max_range=0,
                semaphore_count=per_host,
                verbose=False,
            )
            results = await crawler.arun(f"http://s{h}.localhost:{port}/", config=cfg)
            for r in results:
                if r.success:
                    ok += 1
                    chars += len(r.markdown or "")
                else:
                    err += 1
                    reasons[(r.status_code, (r.error_message or '')[-300:])] += 1
                    print(f"  failed {r.url}", file=sys.stderr)
        await asyncio.gather(*(site(h) for h in range(hosts)))
    s = time.monotonic() - t
    for k, v in raw_errors.most_common(3):
        print(f"  raw error x{v}: {k}", file=sys.stderr)
    for k, v in reasons.most_common(5):
        print(f"  error x{v}: {k}", file=sys.stderr)
    print(f"crawl4ai-{mode}: {ok} pages from {hosts} sites in {s:.2f} s = {ok / s:.0f} pages/s, errors {err}, markdown {chars / 1e6:.1f} MB")

asyncio.run(main())

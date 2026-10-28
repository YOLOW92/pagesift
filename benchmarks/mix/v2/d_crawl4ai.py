"""第 2 版 D：Crawl4AI 的自适应爬取（AdaptiveCrawler.digest，默认策略，max_pages=12），一个“学校 × 方向”跑一次。
结果写成和 pagesift `crawl --out` 相同的 index.jsonl（url、title、status），用 benchmarks/history/0.80.9/grade.py 评分。

    PLAYWRIGHT_BROWSERS_PATH=$MIX/ms-playwright $MIX/py/crawl4ai/bin/python -I benchmarks/mix/v2/d_crawl4ai.py <首页> <查询> <输出目录>

AdaptiveCrawler 在代码里固定了自己的 CrawlerRunConfig，打不开 robots.txt 检查（默认不检查）；这一点和 pagesift 不对称，报告注明。
"""
import asyncio
import json
import sys
import time
from pathlib import Path

from crawl4ai import AdaptiveConfig, AdaptiveCrawler, AsyncWebCrawler


async def main():
    start, query, out = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    out.mkdir(parents=True, exist_ok=True)
    t = time.perf_counter()
    async with AsyncWebCrawler() as crawler:
        ad = AdaptiveCrawler(crawler, AdaptiveConfig(max_pages=12))
        state = await ad.digest(start_url=start, query=query)
    rows = []
    for r in state.knowledge_base:
        md = r.metadata or {}
        rows.append({"url": r.url, "title": md.get("title"), "status": r.status_code})
    (out / "index.jsonl").write_text("".join(json.dumps(x, ensure_ascii=False) + "\n" for x in rows), encoding="utf-8")
    (out / "summary.json").write_text(json.dumps({"pages": len(rows), "wall_s": round(time.perf_counter() - t, 1)}))


if __name__ == "__main__":
    asyncio.run(main())

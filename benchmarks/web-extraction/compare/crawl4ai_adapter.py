#!/usr/bin/env python3
"""Run Crawl4AI on the pagesift bench inputs and write JSON lines for `pagesift bench --external`.

Uses Crawl4AI's documented path for local HTML: the HTTP crawler strategy (no browser) with
`raw:` URLs, cache bypassed and base_url set for link resolution. Writes two outputs:
`raw_markdown` (whole page) and `fit_markdown` (after the pruning content filter).

    benchmarks/data/venvs/crawl4ai/bin/python -I benchmarks/web-extraction/compare/crawl4ai_adapter.py \
        benchmarks/data/inputs/manifest.jsonl benchmarks/data/compare/
"""

import asyncio
import json
import os
import sys
import time
from pathlib import Path

from crawl4ai import AsyncWebCrawler, CacheMode, CrawlerRunConfig
from crawl4ai.async_crawler_strategy import AsyncHTTPCrawlerStrategy
from crawl4ai.markdown_generation_strategy import DefaultMarkdownGenerator

try:
    from crawl4ai.content_filter_strategy import PruningContentFilterLXML as Pruning
except ImportError:
    from crawl4ai.content_filter_strategy import PruningContentFilter as Pruning


async def main(manifest: Path, out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    rows = [json.loads(l) for l in manifest.open() if l.strip()]
    raw_out = (out_dir / "crawl4ai-raw.jsonl").open("w")
    fit_out = (out_dir / "crawl4ai-fit.jsonl").open("w")
    async with AsyncWebCrawler(crawler_strategy=AsyncHTTPCrawlerStrategy(), verbose=False) as crawler:
        for i, row in enumerate(rows, 1):
            if os.environ.get("PAGESIFT_RAW_INPUT") == "1":
                # 原始字节：raw: 只收文本，按常见做法 UTF-8 解码、坏字节替换
                html = Path(row["path"]).read_bytes().decode("utf-8", "replace")
            else:
                html = Path(row["path"]).read_text(encoding="utf-8")
            cfg = CrawlerRunConfig(
                cache_mode=CacheMode.BYPASS,
                base_url=row.get("base_url"),
                markdown_generator=DefaultMarkdownGenerator(content_filter=Pruning()),
                verbose=False,
            )
            start = time.perf_counter()
            try:
                result = await crawler.arun(url="raw:" + html, config=cfg)
                md = result.markdown
                raw, fit = (md.raw_markdown or ""), (md.fit_markdown or "")
            except Exception as e:  # 记录失败，不中断整轮评测
                print(f"{row['id']}: {type(e).__name__}: {e}", file=sys.stderr)
                raw = fit = ""
            ms = (time.perf_counter() - start) * 1000
            for f, text in ((raw_out, raw), (fit_out, fit)):
                f.write(json.dumps({"id": row["id"], "markdown": text, "ms": round(ms, 3)}, ensure_ascii=False) + "\n")
            if i % 100 == 0:
                print(f"  {i}/{len(rows)}", flush=True)
    raw_out.close()
    fit_out.close()


if __name__ == "__main__":
    asyncio.run(main(Path(sys.argv[1]), Path(sys.argv[2])))

#!/usr/bin/env python3
"""Run Python HTML-to-Markdown libraries on the pagesift bench inputs.

Writes JSON lines for `pagesift bench --external`. Each tool is called the way its README shows:

    trafilatura   trafilatura.extract(html, output_format="markdown", url=base_url)
    markitdown    MarkItDown().convert(path).text_content   (whole-page conversion, no extraction)

    bench/data/venvs/pytools/bin/python -I bench/compare/py_adapter.py <trafilatura|markitdown> \
        bench/data/inputs/manifest.jsonl out.jsonl
"""

import json
import sys
import time
from pathlib import Path


def main(tool: str, manifest: Path, out_path: Path) -> None:
    if tool == "trafilatura":
        import trafilatura

        def convert(html: str, path: str, url: str) -> str:
            return trafilatura.extract(html, output_format="markdown", url=url) or ""
    elif tool == "markitdown":
        from markitdown import MarkItDown

        md = MarkItDown()

        def convert(html: str, path: str, url: str) -> str:
            return md.convert(path).text_content or ""
    else:
        raise SystemExit("tool must be trafilatura or markitdown")

    with out_path.open("w") as out:
        for line in manifest.open():
            if not line.strip():
                continue
            row = json.loads(line)
            html = Path(row["path"]).read_text(encoding="utf-8")
            start = time.perf_counter()
            try:
                text = convert(html, row["path"], row.get("base_url") or row["url"])
            except Exception as e:  # 记录失败，不中断整轮评测
                print(f"{row['id']}: {type(e).__name__}: {e}", file=sys.stderr)
                text = ""
            ms = (time.perf_counter() - start) * 1000
            out.write(json.dumps({"id": row["id"], "markdown": text, "ms": round(ms, 3)}, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main(sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3]))

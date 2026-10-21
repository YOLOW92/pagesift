#!/usr/bin/env python3
"""Run Python HTML-to-Markdown libraries on the pagesift bench inputs.

Writes JSON lines for `pagesift bench --external`. Each tool is called the way its README shows:

    trafilatura     trafilatura.extract(html, output_format="markdown", url=base_url)
    markitdown      MarkItDown().convert(path).text_content   (whole-page conversion, no extraction)
    newspaper4k     newspaper.Article(url).download(input_html=html); .parse(); title + text
    goose3          Goose().extract(raw_html=html).cleaned_text
    readability-lxml readability.Document(html).summary() -> markdownify
    justext         paragraphs that are not boilerplate (language stoplist from <html lang>, English default)
    boilerpy3       extractors.ArticleExtractor().get_content(html)
    html2text       html2text.html2text(html)                 (whole-page conversion)
    markdownify     markdownify.markdownify(html)             (whole-page conversion)
    inscriptis      inscriptis.get_text(html)                 (whole-page text)
    resiliparse     resiliparse.extract.html2text.extract_plain_text(html, main_content=True)

    bench/data/venvs/pytools/bin/python -I bench/compare/py_adapter.py <trafilatura|markitdown> \
        bench/data/inputs/manifest.jsonl out.jsonl
"""

import json
import os
import signal
import sys
import time
from pathlib import Path


PAGE_TIMEOUT_S = 30


class PageTimeout(Exception):
    pass


def _alarm(signum, frame):
    raise PageTimeout()


# PAGESIFT_RAW_INPUT=1：编码鲁棒性测试，输入是网站返回的原始字节（不一定是 UTF-8）
RAW = os.environ.get("PAGESIFT_RAW_INPUT") == "1"


def main(tool: str, manifest: Path, out_path: Path) -> None:
    # 某个库在个别页面上卡死时（比如正则回溯），这一页记为空输出，不拖住整轮评测
    signal.signal(signal.SIGALRM, _alarm)
    if tool == "trafilatura":
        import trafilatura

        def convert(html: str, path: str, url: str) -> str:
            return trafilatura.extract(html, output_format="markdown", url=url) or ""
    elif tool == "markitdown":
        from markitdown import MarkItDown

        md = MarkItDown()

        def convert(html: str, path: str, url: str) -> str:
            return md.convert(path).text_content or ""
    elif tool == "newspaper4k":
        import newspaper

        def convert(html: str, path: str, url: str) -> str:
            a = newspaper.Article(url)
            a.download(input_html=html)
            a.parse()
            return (f"# {a.title}\n\n" if a.title else "") + (a.text or "")
    elif tool == "goose3":
        from goose3 import Goose

        g = Goose()

        def convert(html: str, path: str, url: str) -> str:
            a = g.extract(raw_html=html)
            return (f"# {a.title}\n\n" if a.title else "") + (a.cleaned_text or "")
    elif tool == "readability-lxml":
        from markdownify import markdownify
        from readability import Document

        def convert(html: str, path: str, url: str) -> str:
            d = Document(html, url=url)
            body = markdownify(d.summary(html_partial=True), heading_style="ATX")
            title = d.short_title()
            return (f"# {title}\n\n" if title else "") + body
    elif tool == "justext":
        import re

        import justext

        names = {"en": "English", "de": "German", "fr": "French", "es": "Spanish", "it": "Italian", "pt": "Portuguese",
                 "nl": "Dutch", "ru": "Russian", "pl": "Polish", "sv": "Swedish", "tr": "Turkish", "cs": "Czech"}

        def convert(html: str, path: str, url: str) -> str:
            m = re.search(r"<html[^>]*\blang=[\"']?([a-zA-Z]{2})", html[:5000])
            lang = names.get(m.group(1).lower() if m else "en", "English")
            paras = justext.justext(html.encode("utf-8"), justext.get_stoplist(lang))
            return "\n\n".join(p.text for p in paras if not p.is_boilerplate)
    elif tool == "boilerpy3":
        from boilerpy3 import extractors

        ex = extractors.ArticleExtractor(raise_on_failure=False)

        def convert(html: str, path: str, url: str) -> str:
            return ex.get_content(html) or ""
    elif tool == "html2text":
        import html2text

        def convert(html: str, path: str, url: str) -> str:
            h = html2text.HTML2Text(baseurl=url)
            h.body_width = 0
            return h.handle(html)
    elif tool == "markdownify":
        from markdownify import markdownify

        def convert(html: str, path: str, url: str) -> str:
            return markdownify(html, heading_style="ATX")
    elif tool == "inscriptis":
        from inscriptis import get_text

        def convert(html: str, path: str, url: str) -> str:
            return get_text(html)
    elif tool == "resiliparse":
        from resiliparse.extract.html2text import extract_plain_text

        def convert(html: str, path: str, url: str) -> str:
            return extract_plain_text(html, main_content=True)
    else:
        raise SystemExit("unknown tool: " + tool)

    with out_path.open("w") as out:
        for line in manifest.open():
            if not line.strip():
                continue
            row = json.loads(line)
            if RAW:
                # 原始字节：能直接吃字节的工具（trafilatura 自带编码识别）给字节，其余按常见做法 UTF-8 解码、坏字节替换
                data = Path(row["path"]).read_bytes()
                html = data if tool == "trafilatura" else data.decode("utf-8", "replace")
            else:
                html = Path(row["path"]).read_text(encoding="utf-8")
            start = time.perf_counter()
            signal.alarm(PAGE_TIMEOUT_S)
            try:
                text = convert(html, row["path"], row.get("base_url") or row["url"])
            except PageTimeout:
                print(f"{row['id']}: timed out after {PAGE_TIMEOUT_S} s", file=sys.stderr)
                text = ""
            except Exception as e:  # 记录失败，不中断整轮评测
                print(f"{row['id']}: {type(e).__name__}: {e}", file=sys.stderr)
                text = ""
            signal.alarm(0)
            ms = (time.perf_counter() - start) * 1000
            out.write(json.dumps({"id": row["id"], "markdown": text, "ms": round(ms, 3)}, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main(sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3]))

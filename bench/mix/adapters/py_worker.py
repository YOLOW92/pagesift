"""Python 工具的常驻 worker，协议见 README.md：
启动 → stdout 打一行 {"ready":true,...}；之后每读到一行 {"id","input","out"}，结果写进 out，回一行
{"id","ok","ms","bytes","error"}。超时、内存上限、并发由外层 driver.py 管，这里只管转换。

    <工具的 venv>/bin/python -I bench/mix/adapters/py_worker.py <trafilatura|scrapy|crawl4ai|pymupdf4llm|docling|markitdown|extractous>

各工具都用默认参数；唯一的统一改动是“能出 Markdown 的就要 Markdown”（本基准比的就是转 Markdown），
逐个写在 SETUP 里。
"""
import asyncio
import json
import os
import sys
import threading
import time
import traceback
from importlib.metadata import version


def reply(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def jobs():
    for line in sys.stdin:
        if line.strip():
            yield json.loads(line)


def run_sync(tool, ver, convert, with_url=False):
    """一次处理一份：convert(input) -> str；with_url 时 convert(input, url)，url 是网页快照的原始地址（可能为 None）。"""
    reply({"ready": True, "tool": tool, "version": ver, "pid": os.getpid()})
    for job in jobs():
        t = time.perf_counter()
        try:
            text = (convert(job["input"], job.get("url")) if with_url else convert(job["input"])) or ""
            ms = (time.perf_counter() - t) * 1000
            with open(job["out"], "w", encoding="utf-8", errors="surrogatepass") as f:
                f.write(text)
            reply({"id": job["id"], "ok": True, "ms": ms, "bytes": len(text.encode("utf-8", "surrogatepass")), "error": None})
        except Exception as e:  # noqa: BLE001  工具抛什么都记成这份失败
            ms = (time.perf_counter() - t) * 1000
            reply({"id": job["id"], "ok": False, "ms": ms, "bytes": 0, "error": f"{type(e).__name__}: {e}"[:500]})


# ---- 网页正文 ----

def trafilatura_main():
    import trafilatura

    # 给原始字节：trafilatura 自带编码识别。output_format 默认是 txt，这里要 markdown。
    def convert(path, url):
        with open(path, "rb") as f:
            return trafilatura.extract(f.read(), output_format="markdown", url=url)

    run_sync("trafilatura", version("trafilatura"), convert, with_url=True)


# ---- PDF / Office ----

def pymupdf4llm_main():
    # pymupdf4llm 2.0 是空壳包，代码并入了 pymupdf 2.0；按 pymupdf 文档仍从 pymupdf4llm 导入，导不到再用 pymupdf 里的
    try:
        import pymupdf4llm
        to_md = pymupdf4llm.to_markdown
    except ImportError:
        import pymupdf
        to_md = pymupdf.pymupdf4llm.to_markdown
    run_sync("pymupdf4llm", f"pymupdf {version('pymupdf')}", lambda p: to_md(p))


def docling_main():
    from docling.document_converter import DocumentConverter

    conv = DocumentConverter()  # 默认管线：版面模型 + TableFormer，设备自动选择（DOCLING_DEVICE 可覆盖）
    run_sync("docling", version("docling"), lambda p: conv.convert(p).document.export_to_markdown())


def markitdown_main():
    from markitdown import MarkItDown

    md = MarkItDown()  # 默认不启用插件
    run_sync("markitdown", version("markitdown"), lambda p: md.convert(p).text_content)


def extractous_main():
    # 备选：extractous 的 Rust crate 编不出来时，用官方 PyPI 轮子（同一个 Rust 内核 + 预编译的 Tika 原生库）
    from extractous import Extractor

    ex = Extractor()
    run_sync("extractous-py", version("extractous"), lambda p: ex.extract_file_to_string(p)[0])


# ---- 抓取（输入是快照回放服务上的 URL）----

def crawl4ai_main():
    from crawl4ai import AsyncWebCrawler

    async def main():
        async with AsyncWebCrawler() as crawler:  # 默认 BrowserConfig：无头 Chromium（Playwright）
            reply({"ready": True, "tool": "crawl4ai", "version": version("crawl4ai"), "pid": os.getpid()})
            loop = asyncio.get_running_loop()
            q: asyncio.Queue = asyncio.Queue()

            def feed():
                for job in jobs():
                    loop.call_soon_threadsafe(q.put_nowait, job)
                loop.call_soon_threadsafe(q.put_nowait, None)

            threading.Thread(target=feed, daemon=True).start()
            while (job := await q.get()) is not None:
                t = time.perf_counter()
                try:
                    r = await crawler.arun(job["input"])  # 默认 CrawlerRunConfig
                    ms = (time.perf_counter() - t) * 1000
                    if not r.success:
                        reply({"id": job["id"], "ok": False, "ms": ms, "bytes": 0, "error": (r.error_message or "failed")[:500]})
                        continue
                    text = str(r.markdown or "")
                    with open(job["out"], "w", encoding="utf-8") as f:
                        f.write(text)
                    reply({"id": job["id"], "ok": True, "ms": ms, "bytes": len(text.encode()), "error": None})
                except Exception as e:  # noqa: BLE001
                    ms = (time.perf_counter() - t) * 1000
                    reply({"id": job["id"], "ok": False, "ms": ms, "bytes": 0, "error": f"{type(e).__name__}: {e}"[:500]})

    asyncio.run(main())


def scrapy_main():
    # Scrapy 只管抓取，不转 Markdown：输出 response.text（按 Scrapy 识别的编码解码后的 HTML）。
    # 常驻一个 reactor；stdin 线程把每个 URL 交给 CrawlerRunner 跑一个只抓一页的 spider。设置全用 Scrapy 默认值。
    from scrapy.utils.reactor import install_reactor
    from scrapy.settings import Settings

    install_reactor(Settings().get("TWISTED_REACTOR"))
    import scrapy
    from scrapy.crawler import CrawlerRunner
    from twisted.internet import reactor

    runner = CrawlerRunner()

    class One(scrapy.Spider):
        name = "one"

        def __init__(self, job=None, started=None, **kw):
            super().__init__(**kw)
            self.job, self.started, self.done = job, started, False

        async def start(self):
            yield scrapy.Request(self.job["input"], callback=self.parse, errback=self.fail, dont_filter=True)

        def finish(self, ok, text, error):
            if self.done:
                return
            self.done = True
            ms = (time.perf_counter() - self.started) * 1000
            if ok:
                with open(self.job["out"], "w", encoding="utf-8") as f:
                    f.write(text)
            reply({"id": self.job["id"], "ok": ok, "ms": ms, "bytes": len(text.encode()) if ok else 0, "error": error})

        def parse(self, response):
            try:
                text = response.text
            except AttributeError:  # 非文本响应
                return self.finish(False, "", f"non-text response {response.status}")
            self.finish(True, text, None)

        def fail(self, failure):
            self.finish(False, "", repr(failure.value)[:500])

        def closed(self, reason):
            self.finish(False, "", f"closed without response: {reason}")

    def submit(job):
        runner.crawl(One, job=job, started=time.perf_counter())

    def feed():
        for job in jobs():
            reactor.callFromThread(submit, job)
        reactor.callFromThread(lambda: runner.join().addBoth(lambda _: reactor.stop()))

    reply({"ready": True, "tool": "scrapy", "version": version("scrapy"), "pid": os.getpid()})
    threading.Thread(target=feed, daemon=True).start()
    reactor.run(installSignalHandlers=False)


MAINS = {
    "trafilatura": trafilatura_main,
    "scrapy": scrapy_main,
    "crawl4ai": crawl4ai_main,
    "pymupdf4llm": pymupdf4llm_main,
    "docling": docling_main,
    "markitdown": markitdown_main,
    "extractous": extractous_main,
}

if __name__ == "__main__":
    try:
        MAINS[sys.argv[1]]()
    except Exception:
        traceback.print_exc()
        sys.exit(1)

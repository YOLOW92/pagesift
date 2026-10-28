"""Python 工具的常驻 worker，协议见 README.md：
启动 → stdout 打一行 {"ready":true,...}；之后每读到一行 {"id","input","out"}，结果写进 out，回一行
{"id","ok","ms","bytes","error"}。超时、内存上限、并发由外层 driver.py 管，这里只管转换。

    <工具的 venv>/bin/python -I benchmarks/mix/adapters/py_worker.py <trafilatura|scrapy|crawl4ai|pymupdf4llm|docling|markitdown|extractous>

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


def resiliparse_main():
    # 第 2 版。只出纯文本；main_content=True 是 Resiliparse 的正文提取开关（默认 False 是整页转文本，本基准比的是正文）。
    # 解码用 Resiliparse 自己的 detect_encoding + bytes_to_str（它文档里处理原始字节的写法）。
    from resiliparse.extract.html2text import extract_plain_text
    from resiliparse.parse.encoding import bytes_to_str, detect_encoding

    def convert(path):
        with open(path, "rb") as f:
            data = f.read()
        return extract_plain_text(bytes_to_str(data, detect_encoding(data)), main_content=True)

    run_sync("resiliparse", version("resiliparse"), convert)


def newspaper4k_main():
    # 第 2 版。只出纯文本（标题 + 正文）。解码照 newspaper 自己抓网页时的做法：w3lib.html_to_unicode，默认 UTF-8。
    # 和默认值唯一的不同：fetch_images=False。默认 parse() 会联网下载页面上的图片挑头图（image_extractor._fetch_image），
    # 本基准所有工具只读同一份快照、不各自联网（第 1 版 RULES 第三节）；关掉它不影响文字提取。
    # 装的是带语言扩展的 newspaper4k[nlp,zh,ja,th,hi,bn,ta,np]：只装基础包时中文、日文、韩文、阿拉伯文页面直接报 ImportError。
    from newspaper import Article, Config
    from w3lib.encoding import html_to_unicode

    cfg = Config()
    cfg.fetch_images = False

    def convert(path, url):
        with open(path, "rb") as f:
            _, html = html_to_unicode(content_type_header=None, html_body_str=f.read(), default_encoding="utf-8")
        a = Article(url or "https://example.invalid/", config=cfg)
        a.download(input_html=html)
        a.parse()
        return "\n\n".join(x for x in (a.title, a.text) if x)

    run_sync("newspaper4k", version("newspaper4k"), convert, with_url=True)


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


def crawlee_main():
    # 第 2 版。Crawlee 的 HttpCrawler：只抓不转（和 Scrapy 一样输出解码后的 HTML），不开浏览器。
    # 常驻一个 keep_alive 的爬虫，stdin 线程把每个 URL 当一条请求加进队列；其余全用 Crawlee 默认值（重试 3 次、自动并发）。
    from crawlee import ConcurrencySettings, Request  # noqa: F401  ConcurrencySettings 留着对照默认值
    from crawlee.crawlers import HttpCrawler
    from crawlee.storage_clients import MemoryStorageClient

    async def main():
        crawler = HttpCrawler(keep_alive=True, storage_client=MemoryStorageClient(), configure_logging=False)
        pending = {}

        @crawler.router.default_handler
        async def handle(ctx):
            job, started = pending.pop(ctx.request.unique_key)
            ms = (time.perf_counter() - started) * 1000
            body = await ctx.http_response.read()
            text = body.decode(ctx.http_response.headers.get("content-type", "").partition("charset=")[2].split(";")[0].strip() or "utf-8", "replace")
            with open(job["out"], "w", encoding="utf-8") as f:
                f.write(text)
            reply({"id": job["id"], "ok": True, "ms": ms, "bytes": len(text.encode()), "error": None})

        @crawler.failed_request_handler
        async def failed(ctx, error):
            job, started = pending.pop(ctx.request.unique_key)
            reply({"id": job["id"], "ok": False, "ms": (time.perf_counter() - started) * 1000, "bytes": 0, "error": f"{type(error).__name__}: {error}"[:500]})

        loop = asyncio.get_running_loop()
        done = asyncio.Event()

        async def add(job):
            pending[job["id"]] = (job, time.perf_counter())
            await crawler.add_requests([Request.from_url(job["input"], unique_key=job["id"])])

        def feed():
            for job in jobs():
                asyncio.run_coroutine_threadsafe(add(job), loop).result()
            loop.call_soon_threadsafe(done.set)

        async def stop_when_drained():
            await done.wait()
            while pending:
                await asyncio.sleep(0.05)
            crawler.stop()

        reply({"ready": True, "tool": "crawlee", "version": version("crawlee"), "pid": os.getpid()})
        threading.Thread(target=feed, daemon=True).start()
        stopper = asyncio.create_task(stop_when_drained())
        await crawler.run()
        await stopper

    asyncio.run(main())


def kreuzberg_main():
    # 第 2 版。PDF 与 Office：默认配置，输出格式改成 Markdown（统一改动）；关掉结果缓存（默认 use_cache=True，
    # 同一份文件第二遍直接读缓存，测速轮的 1/16 路就不是在测转换了；pagesift serve 同样用 --no-cache）。
    from kreuzberg import ExtractionConfig, extract_file_sync

    cfg = ExtractionConfig(output_format="markdown", use_cache=False)
    run_sync("kreuzberg", version("kreuzberg"), lambda p: extract_file_sync(p, config=cfg).content)


def mineru_main(tier=None):
    # 第 2 版。照 `mineru-kit parse` 的本地路径在进程内调用（常驻，不每份起进程）：mineru.parser.parse(path, tier=…)，
    # 取 result.markdown()。tier 不给就照 mineru-kit 的默认：PDF 用 standard（默认安装 = ONNX + llama.cpp Vulkan），
    # Office/EPUB/CSV/HTML 只能 flash。mineru-flash 是官方文档里“最快、不用模型”的档（RULES 第四节注明的推荐配置）。
    from pathlib import Path

    from mineru.filetypes import is_flash_only_parse_extension
    from mineru.parser import parse as local_parse

    def convert(path):
        p = Path(path)
        t = "flash" if is_flash_only_parse_extension(p) else (tier or "standard")
        return local_parse(p, tier=t, ocr_mode="auto", image_analysis=True, page_range="").markdown()

    run_sync("mineru" if tier is None else f"mineru-{tier}", version("mineru"), convert)


def marker_main():
    # 第 2 版。Marker 的 Python 用法（README）：PdfConverter(artifact_dict=create_model_dict())，text_from_rendered 取 Markdown。
    # 设备由 TORCH_DEVICE 指定（cpu / cuda），同 Docling 的做法；其余默认（不开 LLM）。
    from marker.converters.pdf import PdfConverter
    from marker.models import create_model_dict
    from marker.output import text_from_rendered

    conv = PdfConverter(artifact_dict=create_model_dict())
    run_sync("marker", version("marker-pdf"), lambda p: text_from_rendered(conv(p))[0])


def firecrawl_main():
    # 第 2 版。Firecrawl 自托管（官方镜像，默认配置）：POST /v2/scrape {url, formats: ["markdown"]}，其余默认
    # （onlyMainContent 默认 true）。只用标准库发请求；渲染、抓取都在 Firecrawl 的容器里。
    import urllib.error
    import urllib.request

    base = os.environ["MIX_FIRECRAWL_URL"].rstrip("/")

    def convert(url):
        req = urllib.request.Request(base + "/v2/scrape", data=json.dumps({"url": url, "formats": ["markdown"]}).encode(),
                                     headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                doc = json.loads(r.read())
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"HTTP {e.code}: {e.read()[:300]!r}") from None
        if not doc.get("success"):
            raise RuntimeError(json.dumps(doc.get("error") or doc, ensure_ascii=False)[:400])
        return (doc.get("data") or {}).get("markdown") or ""

    run_sync("firecrawl", "self-hosted", convert)


def pandoc_main():
    # 第 2 版。pandoc 是独立二进制（MIX_PANDOC），每份一个进程：按扩展名选读入格式（pandoc 默认行为），输出 GitHub 风格 Markdown。
    import subprocess

    exe = os.environ["MIX_PANDOC"]
    ver = subprocess.run([exe, "--version"], capture_output=True, text=True).stdout.split("\n", 1)[0]

    def convert(path):
        r = subprocess.run([exe, "-t", "gfm", path], capture_output=True)
        if r.returncode != 0:
            raise RuntimeError(r.stderr.decode("utf-8", "replace").strip()[:400] or f"exit {r.returncode}")
        return r.stdout.decode("utf-8", "replace")

    run_sync("pandoc", ver, convert)


MAINS = {
    "resiliparse": resiliparse_main,
    "newspaper4k": newspaper4k_main,
    "crawlee": crawlee_main,
    "kreuzberg": kreuzberg_main,
    "mineru": mineru_main,
    "mineru-flash": lambda: mineru_main("flash"),
    "marker": marker_main,
    "firecrawl": firecrawl_main,
    "pandoc": pandoc_main,
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

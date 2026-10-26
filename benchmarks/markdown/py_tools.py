"""Python 转换器：把一个目录里的 NNN.html 转成 <输出目录>/<工具>/NNN.md，打印每页平均耗时。

python -I py_tools.py <html 目录> <输出目录> <工具>...
工具：markdownify、html2text（GPL，只做评测）、markitdown、crawl4ai（在 Crawl4AI 的环境里跑）。
都用常见用法：ATX 标题、围栏代码块；Crawl4AI 用默认的 DefaultMarkdownGenerator，取 raw_markdown（不带引用编号）。
"""
import io, os, sys, time


def converter(name):
    if name == "markdownify":
        from markdownify import markdownify
        return lambda h: markdownify(h, heading_style="ATX", code_language_callback=_lang, bullets="-")
    if name == "html2text":
        import html2text
        def conv(h):
            p = html2text.HTML2Text()
            p.body_width = 0
            p.mark_code = False
            return p.handle(h)
        return conv
    if name == "markitdown":
        from markitdown import MarkItDown, StreamInfo
        md = MarkItDown()
        return lambda h: md.convert_stream(io.BytesIO(h.encode()), stream_info=StreamInfo(extension=".html", mimetype="text/html", charset="utf-8")).text_content
    if name == "crawl4ai":
        from crawl4ai.markdown_generation_strategy import DefaultMarkdownGenerator
        g = DefaultMarkdownGenerator()
        return lambda h: g.generate_markdown(h, citations=False).raw_markdown
    raise SystemExit(f"unknown tool {name}")


def _lang(el):
    for c in el.get("class", []) or []:
        if c.startswith("language-"):
            return c[9:]
    code = el.find("code")
    if code is not None:
        for c in code.get("class", []) or []:
            if c.startswith("language-"):
                return c[9:]
    return None


def main():
    src, out = sys.argv[1], sys.argv[2]
    files = sorted(f for f in os.listdir(src) if f.endswith(".html"))
    for tool in sys.argv[3:]:
        conv = converter(tool)
        d = os.path.join(out, tool)
        os.makedirs(d, exist_ok=True)
        total = 0.0
        for f in files:
            h = open(os.path.join(src, f), encoding="utf-8").read()
            t = time.perf_counter()
            try:
                md = conv(h)
            except Exception as e:
                md = ""
                print(f"{tool} {f}: {e}", file=sys.stderr)
            total += time.perf_counter() - t
            open(os.path.join(d, f[:-5] + ".md"), "w", encoding="utf-8").write(md or "")
        print(f"{tool}: {1000 * total / len(files):.2f} ms/page")


main()

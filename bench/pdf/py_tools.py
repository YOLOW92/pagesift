"""Python PDF 转换器：把目录（递归）里的每个 PDF 转成 Markdown/文本，按 olmOCR-bench 的命名写出
<输出目录>/<工具>/<相对路径去掉 .pdf>_pg1_repeat1.md，打印每个文件的平均耗时。

python -I py_tools.py <pdf 目录> <输出目录> <工具>...
工具：pymupdf4llm（AGPL，只做评测）、pdfplumber、pypdf、markitdown（底层 pdfminer.six）。都用默认参数。
"""
import glob, io, os, sys, time


def make(name):
    if name == "pymupdf4llm":
        import pymupdf4llm
        return lambda p: pymupdf4llm.to_markdown(p, show_progress=False)
    if name == "pdfplumber":
        import pdfplumber
        def f(p):
            with pdfplumber.open(p) as pdf:
                return "\n\n".join(page.extract_text() or "" for page in pdf.pages)
        return f
    if name == "pypdf":
        from pypdf import PdfReader
        return lambda p: "\n\n".join(page.extract_text() or "" for page in PdfReader(p).pages)
    if name == "markitdown":
        from markitdown import MarkItDown
        md = MarkItDown()
        return lambda p: md.convert(p).text_content
    raise SystemExit(f"unknown tool {name}")


def main():
    src, out = sys.argv[1], sys.argv[2]
    files = sorted(glob.glob(os.path.join(src, "**/*.pdf"), recursive=True))
    for tool in sys.argv[3:]:
        conv = make(tool)
        total, failed = 0.0, 0
        for f in files:
            t = time.perf_counter()
            try:
                text = conv(f) or ""
            except Exception as e:
                text = ""
                failed += 1
            total += time.perf_counter() - t
            rel = os.path.splitext(os.path.relpath(f, src))[0]
            dst = os.path.join(out, tool, rel + "_pg1_repeat1.md")
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            open(dst, "w", encoding="utf-8").write(text)
        print(f"{tool}: {len(files)} files, {failed} failed, {1000 * total / len(files):.2f} ms/file", flush=True)


main()

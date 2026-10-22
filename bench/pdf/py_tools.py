"""Python PDF 转换器：把目录（递归）里的每个 PDF 转成 Markdown/文本，按 olmOCR-bench 的命名写出
<输出目录>/<工具>/<相对路径去掉 .pdf>_pg1_repeat1.md，打印每个文件的平均耗时。

python -I py_tools.py <pdf 目录> <输出目录> <工具>...
工具：pymupdf4llm（AGPL，只做评测）、pdfplumber、pypdf、markitdown（底层 pdfminer.six）。都用默认参数。
"""
import glob, io, os, sys, time
from concurrent.futures import ProcessPoolExecutor

_CONV = None
_SRC = None
_OUT = None
_TOOL = None


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


def _init_worker(tool, src, out):
    global _CONV, _SRC, _OUT, _TOOL
    os.environ["PAGESIFT_ONE_CORE"] = "1"
    os.environ["OMP_NUM_THREADS"] = "1"
    _TOOL, _SRC, _OUT = tool, src, out
    _CONV = make(tool)


def _proc_file(f):
    t = time.perf_counter()
    failed = 0
    try:
        text = _CONV(f) or ""
    except Exception:
        text = ""
        failed = 1
    dt = time.perf_counter() - t
    rel = os.path.splitext(os.path.relpath(f, _SRC))[0]
    dst = os.path.join(_OUT, _TOOL, rel + "_pg1_repeat1.md")
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    open(dst, "w", encoding="utf-8").write(text)
    return dt, failed


def main():
    src, out = sys.argv[1], sys.argv[2]
    files = sorted(glob.glob(os.path.join(src, "**/*.pdf"), recursive=True))
    workers = 1 if os.environ.get("PAGESIFT_ONE_CORE") == "1" else min(6, os.cpu_count() or 4)
    for tool in sys.argv[3:]:
        total, failed = 0.0, 0
        if workers > 1:
            with ProcessPoolExecutor(max_workers=workers, initializer=_init_worker, initargs=(tool, src, out)) as ex:
                for dt, fl in ex.map(_proc_file, files, chunksize=8):
                    total += dt
                    failed += fl
        else:
            _init_worker(tool, src, out)
            for f in files:
                dt, fl = _proc_file(f)
                total += dt
                failed += fl
        print(f"{tool}: {len(files)} files, {failed} failed, {1000 * total / len(files):.2f} ms/file", flush=True)


if __name__ == "__main__":
    main()

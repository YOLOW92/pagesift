#!/usr/bin/env python3
"""D3 PDF 提取的 Python 竞品适配器（单进程；并发由外层 16 分片实现）。

用法: d3_py.py --tool pymupdf4llm|pypdf|pdfplumber|pdfminer|markitdown \
               --manifest m.jsonl --out o.jsonl
manifest 每行 {"id","pdf"}（pdf 为容器内路径）；输出 {"id","markdown","ms"}
"""
import argparse, json, sys, time

def convert(tool, path):
    if tool == "pymupdf4llm":
        import pymupdf4llm
        return pymupdf4llm.to_markdown(path)
    if tool == "pypdf":
        from pypdf import PdfReader
        r = PdfReader(path)
        return "\n\n".join((p.extract_text() or "") for p in r.pages)
    if tool == "pdfplumber":
        import pdfplumber
        out = []
        with pdfplumber.open(path) as pdf:
            for p in pdf.pages:
                out.append(p.extract_text() or "")
        return "\n\n".join(out)
    if tool == "pdfminer":
        from pdfminer.high_level import extract_text
        return extract_text(path)
    if tool == "markitdown":
        from markitdown import MarkItDown
        return MarkItDown().convert(path).text_content or ""
    raise SystemExit("unknown tool " + tool)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tool", required=True)
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    n = err = empty = 0
    with open(a.out, "w", encoding="utf-8") as o:
        for line in open(a.manifest, encoding="utf-8"):
            if not line.strip():
                continue
            row = json.loads(line)
            t0 = time.perf_counter()
            try:
                md = convert(a.tool, row["pdf"])
                e = None
            except Exception as ex:
                md, e = "", f"{type(ex).__name__}: {ex}"
            ms = (time.perf_counter() - t0) * 1000
            n += 1
            if e: err += 1
            if not (md or "").strip(): empty += 1
            rec = {"id": row["id"], "markdown": md, "ms": round(ms, 3)}
            if e: rec["error"] = e
            o.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(json.dumps({"tool": a.tool, "n": n, "errors": err, "empty": empty}), file=sys.stderr)

if __name__ == "__main__":
    main()

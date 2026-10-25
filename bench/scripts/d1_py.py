#!/usr/bin/env python3
"""D1 正文提取的 Python 竞品适配器（多进程，避免 GIL 成为瓶颈）。

用法: d1_py.py --tool trafilatura --manifest /data/showdown/d1/manifest.jsonl \
               --out /out/trafilatura.jsonl --workers 16

输出每行 {"id","tool","markdown","ms"}；markdown 统一成文本，便于同一评分器比较。
驱动侧额外记录自身 CPU 时间，用于证明"不是 Python 驱动成了瓶颈"。
"""
import argparse, json, os, resource, sys, time
from multiprocessing import Pool

TOOL = None

def setup(tool):
    global TOOL
    TOOL = tool

def extract(html: str) -> str:
    t = TOOL
    if t == "trafilatura":
        import trafilatura
        return trafilatura.extract(html, output_format="markdown", include_comments=False,
                                   include_tables=True) or ""
    if t == "readability-lxml":
        from readability import Document
        import html2text
        summary = Document(html).summary()
        return html2text.html2text(summary)
    if t == "goose3":
        from goose3 import Goose
        return Goose().extract(raw_html=html).cleaned_text or ""
    if t == "boilerpy3":
        from boilerpy3 import extractors
        return extractors.ArticleExtractor().get_content(html) or ""
    if t == "justext":
        import justext
        stops = justext.get_stoplist("English")
        paras = justext.justext(html, stops)
        return "\n\n".join(p.text for p in paras if not p.is_boilerplate)
    if t == "resiliparse":
        from resiliparse.extract.html2text import extract_plain_text
        return extract_plain_text(html, main_content=True, preserve_formatting=False)
    if t == "html2text":
        import html2text
        return html2text.html2text(html)
    if t == "markdownify":
        from markdownify import markdownify
        return markdownify(html)
    raise SystemExit(f"unknown tool {t}")

HOST_DATA = "/home/yolow92/pagesift/bench/data"

def resolve(path: str) -> str:
    """容器内 /data/... 直接可用；在宿主机上跑时映射回真实路径。"""
    if os.path.exists(path):
        return path
    if path.startswith("/data/"):
        alt = os.path.join(HOST_DATA, path[len("/data/"):])
        if os.path.exists(alt):
            return alt
    return path

def work(row):
    try:
        html = open(resolve(row["path"]), encoding="utf-8", errors="replace").read()
    except Exception as e:
        return {"id": row["id"], "tool": TOOL, "markdown": "", "ms": 0.0, "error": str(e)}
    t0 = time.perf_counter()
    try:
        md = extract(html)
        err = None
    except Exception as e:
        md, err = "", f"{type(e).__name__}: {e}"
    ms = (time.perf_counter() - t0) * 1000.0
    r = {"id": row["id"], "tool": TOOL, "markdown": md, "ms": round(ms, 3)}
    if err:
        r["error"] = err
    return r

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tool", required=True)
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--workers", type=int, default=16)
    a = ap.parse_args()

    rows = [json.loads(l) for l in open(a.manifest, encoding="utf-8") if l.strip()]
    os.makedirs(os.path.dirname(a.out), exist_ok=True)
    t_start = time.perf_counter()
    cpu0 = resource.getrusage(resource.RUSAGE_SELF)
    n_err = n_empty = 0

    with Pool(a.workers, initializer=setup, initargs=(a.tool,)) as pool, \
         open(a.out, "w", encoding="utf-8") as f:
        for r in pool.imap(work, rows, chunksize=16):
            if r.get("error"):
                n_err += 1
            if not (r["markdown"] or "").strip():
                n_empty += 1
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

    wall = time.perf_counter() - t_start
    cpu1 = resource.getrusage(resource.RUSAGE_SELF)
    kid = resource.getrusage(resource.RUSAGE_CHILDREN)
    driver_cpu = (cpu1.ru_utime - cpu0.ru_utime) + (cpu1.ru_stime - cpu0.ru_stime)
    tool_cpu = kid.ru_utime + kid.ru_stime
    print(json.dumps({
        "tool": a.tool, "pages": len(rows), "workers": a.workers,
        "wall_s": round(wall, 3), "pages_per_s": round(len(rows)/wall, 2),
        "errors": n_err, "empty": n_empty,
        "driver_cpu_s": round(driver_cpu, 2), "children_cpu_s": round(tool_cpu, 2),
        "driver_share_pct": round(100*driver_cpu/max(driver_cpu+tool_cpu, 1e-9), 2),
    }, ensure_ascii=False), file=sys.stderr)

if __name__ == "__main__":
    main()

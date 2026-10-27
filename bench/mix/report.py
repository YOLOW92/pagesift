"""Build the English Markdown tables for docs/benchmarks.md from the mix-benchmark results.

    python3 -I bench/mix/report.py > bench/mix/results/report-2026-10-27.md

Reads bench/mix/results/quality-<sub>.json (score_quality.py), the per-tool summary.json of the quality run
($MIX/runs/quality) for versions, and the speed run ($MIX/runs/speed/w<N>/<sub>/<tool>/results.jsonl).
Every number carries its direction (↑ higher is better, ↓ lower is better).
"""
import json
import os
import statistics
from pathlib import Path

HERE = Path(__file__).resolve().parent
RES = HERE / "results"
MIX = Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools"))
DATE = "2026-10-27"
NAME = {"pagesift": "**pagesift**", "pagesift-file": "**pagesift**", "pagesift-crawl": "**pagesift**", "readability": "Readability + Turndown",
        "crawl4ai": "Crawl4AI", "scrapy": "Scrapy", "markitdown": "MarkItDown", "docling": "Docling (CPU)", "docling-gpu": "Docling (GPU, cu126)"}


def q(sub):
    p = RES / f"quality-{sub}.json"
    return json.loads(p.read_text()) if p.exists() else None


def version(sub, tool):
    p = MIX / "runs/quality" / sub / tool / "summary.json"
    try:
        v = json.loads(p.read_text()).get("versions") or []
        return ", ".join(v)
    except (OSError, ValueError):
        return ""


def name(t):
    return NAME.get(t, t)


def f3(x):
    return "—" if x is None else f"{x:.3f}"


def fails(r):
    s = r["status"]
    return sum(s.get(k, 0) for k in ("empty", "error", "crash", "oom")), s.get("timeout", 0)


def speed(sub, tool, w):
    d = MIX / "runs/speed" / f"w{w}" / sub / tool
    try:
        rows = [json.loads(l) for l in (d / "results.jsonl").read_text().splitlines()]
        summ = json.loads((d / "summary.json").read_text())
    except (OSError, ValueError):
        return None
    ms = sorted(r["ms"] for r in rows if r.get("status") == "ok" and r.get("ms") is not None)
    peaks = [r.get("peak_mb", 0) for r in rows]
    if not ms:
        return None
    return {"p50": statistics.median(ms), "p95": ms[min(len(ms) - 1, int(0.95 * len(ms)))], "n": len(rows),
            "tput": len(rows) / summ["wall_s"] if summ.get("wall_s") else None, "peak": max(peaks) if peaks else None,
            "live": summ.get("peak_live_workers")}


def speed_table(sub, tools, ws, title, note=""):
    print(f"\n**Speed — {title}** (quiet machine, one tool at a time under the shared bench lock)\n")
    head = "| Tool | " + " | ".join(f"p50 ms/item ↓ ({w} worker{'s' if w > 1 else ''})" for w in ws[:1]) + " | p95 ms/item ↓ | "
    head += " | ".join(f"items/s ↑ ({w} w)" for w in ws) + " | peak MB/worker ↓ | workers actually live at max |"
    print(head)
    print("|" + "---|" * (head.count("|") - 1))
    for t in tools:
        s1 = speed(sub, t, ws[0])
        cells = [name(t), "—" if not s1 else f"{s1['p50']:.1f}", "—" if not s1 else f"{s1['p95']:.1f}"]
        for w in ws:
            s = speed(sub, t, w)
            cells.append("—" if not s or not s["tput"] else f"{s['tput']:.1f}")
        top = speed(sub, t, ws[-1])
        cells.append("—" if not s1 else f"{s1['peak']:.0f}")
        cells.append("—" if not top else str(top["live"]))
        print("| " + " | ".join(cells) + " |")
    if note:
        print(f"\n{note}")


def status_rows(d, tools, extra=None):
    for t in tools:
        r = d["tools"].get(t)
        if not r:
            continue
        f, to = fails(r)
        row = [name(t), version(d["sub"], t), str(r["items"]), f3(r["completion"]), str(f), str(to)]
        if extra:
            row += extra(t, r)
        print("| " + " | ".join(row) + " |")


def score(d, group, set_, t, key="all_zero_fail"):
    try:
        return d["scores"][group][set_][t][key]
    except KeyError:
        return None


def main():
    print(f"# Mixed benchmark (self-built), {DATE}\n")
    print("Rules, seeds and frozen sample lists: `bench/mix/RULES.md` (frozen at f1a0cdf3). All tools run with default "
          "parameters (the only common change: ask for Markdown where a tool can produce it). Machine: i7-10700 "
          "(8C/16T), 15 GB RAM, GTX 1070. *Score with failures = 0*: every sampled item counts, an item a tool did not "
          "finish (timeout, crash, out of memory, empty output) scores 0. *Common items*: only items every compared tool "
          "finished. Raw per-item results: `bench/mix/results/quality-<subset>.json`.\n")

    # 1. Web extraction
    a1, a2, a4 = q("a1"), q("a2"), q("a4")
    web = ["pagesift", "crw", "trafilatura", "readability"]
    print("## 1. Main-content extraction (HTML snapshots)\n")
    print("| Tool | Version | AEB F1 ↑ | WCXB F1 ↑ | WebMainBench F1 ↑ | Firecrawl truth recall ↑ | A2 random pages: completion ↑ | A2: pages with mojibake ↓ | A4 hard pages: completion ↑ | A4 timeouts ↓ |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for t in web:
        r2 = a2["tools"].get(t, {}) if a2 else {}
        r4 = a4["tools"].get(t, {}) if a4 else {}
        print("| " + " | ".join([name(t), version("a1", t), f3(score(a1, "all", "AEB", t)), f3(score(a1, "all", "WCXB", t)),
                                 f3(score(a1, "all", "WebMainBench", t)), f3(score(a1, "all", "Firecrawl", t)),
                                 f3(r2.get("completion")), str(r2.get("garble_pages", "—")), f3(r4.get("completion")),
                                 str(r4.get("status", {}).get("timeout", 0))]) + " |")
    print("\nA1: 150 pages per set sampled with fixed seeds (AEB, WCXB test split, WebMainBench, Firecrawl "
          "scrape-content-dataset-v1), each scored with its own scorer; failures score 0. A2: 269 of 500 sampled Tranco "
          "pages could be fetched (robots.txt respected). A4: 166 adversarial pages (encoding traps, broken HTML, >5 MB "
          "pages, block pages, deep nesting); completion = non-empty output within 30 s.")
    speed_table("a2", web, [1, 4, 16], "main-content extraction on the 269 A2 pages",
                "pagesift is run as the released CLI, one process per page (`pagesift convert`), so its per-item time "
                "includes process start-up; the other tools are resident workers.")

    # 2. Crawling
    crawl = ["pagesift-crawl", "spider", "scrapy", "crawl4ai"]
    print("\n## 2. Fetching (crawler tools on a local replay of the A2 snapshots)\n")
    print("| Tool | Version | Pages | Completion ↑ | Failures ↓ | Timeouts ↓ | Pages with mojibake ↓ |")
    print("|---|---|---|---|---|---|---|")
    status_rows(a2, crawl, lambda t, r: [str(r.get("garble_pages", "—"))])
    print("\nNo content-quality score here (Scrapy returns raw HTML, the others whole-page Markdown). pagesift refuses "
          "pages it identifies as access barriers (4 of its 5 errors: captcha / challenge / block pages) instead of "
          "returning the barrier as content; that counts as a failure here. pagesift needs `--allow-cidr 127.0.0.1/32` "
          "to fetch from the loopback replay server (its SSRF guard blocks private addresses by default).")
    speed_table("a2", crawl, [1, 4, 16], "fetching the 269 A2 pages from the replay server")

    # 3. PDF
    b1, b2, b3 = q("b1"), q("b2"), q("b3")
    pdf = ["pagesift-file", "pdf_oxide", "pymupdf4llm"]
    print("\n## 3. PDF\n")
    print("**olmOCR-bench, born-digital subsets (B1), score with failures = 0**\n")
    print("| Tool | Version | Pages | olmOCR score ↑ (official, incl. baseline) | Without baseline ↑ | Failures ↓ | Timeouts ↓ |")
    print("|---|---|---|---|---|---|---|")
    for t in pdf:
        r = b1["tools"][t]
        f, to = fails(r)
        print("| " + " | ".join([name(t), version("b1", t), str(r["items"]), f3(score(b1, "all", "olmOCR", t)),
                                 f3(score(b1, "all", "olmOCR 去掉 baseline", t)), str(f), str(to)]) + " |")
    print("\n**Same, on the fixed 10 % model subset (144 pages) so that Docling can be compared**\n")
    print("| Tool | olmOCR score ↑ (official) | Without baseline ↑ | Common items only ↑ (n) |")
    print("|---|---|---|---|")
    for t in pdf + ["docling"]:
        print("| " + " | ".join([name(t), f3(score(b1, "model_subset", "olmOCR", t)), f3(score(b1, "model_subset", "olmOCR 去掉 baseline", t)),
                                 f"{f3(score(b1, 'model_subset', 'olmOCR', t, 'common_only'))} ({score(b1, 'model_subset', 'olmOCR', t, 'n_common')})"]) + " |")
    print("\nThe official score is olmOCR's `benchmark.py` Average Score of one run over the five born-digital subsets "
          "(mean of per-category pass rates, including the baseline category it adds per PDF). The older table in this "
          "document (64.4 for pymupdf4llm 1.28.2) averaged five separate per-subset runs and is not comparable.\n")
    print("**Random real PDFs (B2: 71 of 100 sampled arXiv / GOV.UK files) and hard PDFs (B3: 100 pdf.js test files)**\n")
    print("| Tool | B2 text completeness ↑ | B2 failures ↓ | B3 completion ↑ | B3 failures ↓ |")
    print("|---|---|---|---|---|")
    for t in pdf + ["docling"]:
        r2, r3 = b2["tools"].get(t), b3["tools"].get(t)
        g = "model_subset" if t == "docling" else "all"
        print("| " + " | ".join([name(t) + (" (10 % subset)" if t == "docling" else ""), f3(score(b2, g, "B2", t)),
                                 str(fails(r2)[0] + fails(r2)[1]) if r2 else "—", f3(r3["completion"]) if r3 else "—",
                                 str(fails(r3)[0] + fails(r3)[1]) if r3 else "—"]) + " |")
    print("\nB2 text completeness: share of the words of the PDF's text layer (read by poppler `pdftotext`, which is not "
          "a competitor) that appear in the output. Docling ran only the fixed 10 % subset (5 B2 files, 8 B3 files), so "
          "its numbers there are noisy.")
    speed_table("b1", pdf + ["docling", "docling-gpu"], [1, 16], "PDF, same 144 pages (B1 10 % subset) for every tool",
                "Docling GPU uses the PyTorch cu126 build (the default PyPI build has no kernels for the GTX 1070) and is "
                "capped at 2 workers by GPU memory. pagesift runs as one CLI process per file.")

    # 4. Office
    b4 = q("b4")
    off = ["pagesift-file", "markitdown", "extractous"]
    print("\n## 4. Office documents and e-books (B4)\n")
    print("| Tool | Version | Files | Structure kept ↑ dev | Structure kept ↑ holdout | Completion ↑ | Failures ↓ |")
    print("|---|---|---|---|---|---|---|")
    for t in off:
        r = b4["tools"][t]
        print("| " + " | ".join([name(t), version("b4", t), str(r["items"]), f3(score(b4, "all", "B4 dev", t)),
                                 f3(score(b4, "all", "B4 holdout", t)), f3(r["completion"]), str(sum(fails(r)))]) + " |")
    print("\nDocling on the fixed 10 % subset (7 files): structure kept "
          f"{f3(score(b4, 'model_subset', 'B4 dev', 'docling'))} dev / {f3(score(b4, 'model_subset', 'B4 holdout', 'docling'))} holdout "
          f"(pagesift on the same files {f3(score(b4, 'model_subset', 'B4 dev', 'pagesift-file'))} / "
          f"{f3(score(b4, 'model_subset', 'B4 holdout', 'pagesift-file'))}). extractous outputs plain text only, and its "
          "embedded Tika failed on 26 of 81 files (deterministic, same with the PyPI wheel).")
    speed_table("b4", off + ["docling"], [1, 16], "Office and e-books, 81 files (Docling: its 7-file subset)")


if __name__ == "__main__":
    main()

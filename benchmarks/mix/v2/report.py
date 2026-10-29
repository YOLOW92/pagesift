"""Mix v2: build the English report from the v2 results.

    python3 -I benchmarks/mix/v2/report.py > benchmarks/mix/v2/results/report-2026-10-28.md

Reads v2/results/quality-<sub>.json (score_quality.py --out v2/results), the per-tool summary.json of the quality run
($MIX/runs-v2/quality) for versions, and the speed run ($MIX/runs-v2/speed/w<N>/<sub>/<tool>/). Every number carries
its direction (↑ higher is better, ↓ lower is better).
"""
import json
import os
import statistics
from pathlib import Path

HERE = Path(__file__).resolve().parent
RES = HERE / "results"
MIX = Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools"))
RUNS = MIX / "runs-v2"
DATE = "2026-10-28"
NAME = {
    "pagesift": "**pagesift 0.80.9**", "pagesift-file": "**pagesift 0.80.9**", "pagesift-crawl": "**pagesift 0.80.9**",
    "pagesift-080": "pagesift 0.80.0", "pagesift-080-file": "pagesift 0.80.0", "pagesift-080-crawl": "pagesift 0.80.0",
    "pagesift-serve": "**pagesift 0.80.9 serve**", "pagesift-serve-file": "**pagesift 0.80.9 serve**", "pagesift-serve-crawl": "**pagesift 0.80.9 serve**",
    "pagesift-081": "**pagesift 0.81**", "pagesift-081-file": "**pagesift 0.81**", "pagesift-081-crawl": "**pagesift 0.81**",
    "pagesift-081-serve": "**pagesift 0.81 serve**", "pagesift-081-serve-file": "**pagesift 0.81 serve**", "pagesift-081-serve-crawl": "**pagesift 0.81 serve**",
    "readability": "Readability + Turndown", "resiliparse": "Resiliparse", "newspaper4k": "newspaper4k", "crawlee": "Crawlee",
    "crawl4ai": "Crawl4AI", "scrapy": "Scrapy", "markitdown": "MarkItDown", "kreuzberg": "Kreuzberg", "pandoc": "pandoc",
    "mineru": "MinerU (default tier)", "mineru-flash": "MinerU (flash tier)", "docling": "Docling (CPU)", "docling-gpu": "Docling (GPU, cu126)",
}


def q(sub):
    p = RES / f"quality-{sub}.json"
    return json.loads(p.read_text()) if p.exists() else None


def version(sub, tool):
    try:
        return ", ".join(json.loads((RUNS / "quality" / sub / tool / "summary.json").read_text()).get("versions") or [])
    except (OSError, ValueError):
        return ""


def name(t):
    return NAME.get(t, t)


def f3(x):
    return "—" if x is None else f"{x:.3f}"


def fails(r):
    s = r["status"]
    return sum(s.get(k, 0) for k in ("empty", "error", "crash", "oom", "stopped")), s.get("timeout", 0)


def speed(sub, tool, w):
    d = RUNS / "speed" / f"w{w}" / sub / tool
    try:
        rows = [json.loads(l) for l in (d / "results.jsonl").read_text().splitlines()]
        summ = json.loads((d / "summary.json").read_text())
    except (OSError, ValueError):
        return None
    ms = sorted(r["ms"] for r in rows if r.get("status") == "ok" and r.get("ms") is not None)
    peaks = [r.get("peak_mb") or 0 for r in rows]
    if not ms:
        return None
    return {"p50": statistics.median(ms), "p95": ms[min(len(ms) - 1, int(0.95 * len(ms)))], "n": len(rows),
            "ok": sum(r.get("status") == "ok" for r in rows),
            "tput": len(rows) / summ["wall_s"] if summ.get("wall_s") else None, "peak": max(peaks) if peaks else None,
            "live": summ.get("peak_live_workers")}


def speed_table(sub, tools, ws, title, note=""):
    print(f"\n**Speed — {title}** (quiet machine, one tool at a time under the shared bench lock)\n")
    head = f"| Tool | p50 ms/item ↓ ({ws[0]} worker) | p95 ms/item ↓ | " + " | ".join(f"items/s ↑ ({w} w)" for w in ws)
    head += " | peak MB/worker ↓ | workers live at max |"
    print(head)
    print("|" + "---|" * (head.count("|") - 1))
    for t in tools:
        s1 = speed(sub, t, ws[0])
        if s1 is None and all(speed(sub, t, w) is None for w in ws):
            continue
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


def score(d, group, set_, t, key="all_zero_fail"):
    try:
        return d["scores"][group][set_][t][key]
    except (KeyError, TypeError):
        return None


def main():
    print(f"# Mixed benchmark v2 (self-built), {DATE}\n")
    print("Rules: `benchmarks/mix/v2/RULES.md` (frozen at f0ca38e3; everything not changed there follows v1, "
          "`benchmarks/mix/RULES.md`). Samples are the frozen v1 samples. pagesift 0.80.9 is built from 33ff97ab with the "
          "release workflow's command; 0.80.0 is the release binary, re-run on the same machine. All tools run with default "
          "parameters (the only common change: ask for Markdown where a tool can produce it); every other deviation is listed "
          "in `benchmarks/mix/v2/tools.lock.md`. Machine: i7-10700 (8C/16T), 15 GB RAM, GTX 1070. *Score with failures = 0*: "
          "every sampled item counts, an item a tool did not finish (timeout, crash, out of memory, empty output, not reached) "
          "scores 0. *Common items*: only items every compared tool finished.\n")

    # 1. Web extraction
    a1, a2, a4 = q("a1"), q("a2"), q("a4")
    web = ["pagesift", "pagesift-080", "crw", "trafilatura", "readability", "resiliparse", "newspaper4k"]
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
    print("\nA1: 150 pages per set (AEB, WCXB test split, WebMainBench, Firecrawl), each scored with its own scorer. A2: the 269 "
          "fetched Tranco pages. A4: 151 of the 166 v1 adversarial pages (15 captured block pages were lost before this run, "
          "list in `v2/samples/a4-missing.tsv`); completion = non-empty output within 30 s. Resiliparse and newspaper4k output "
          "plain text only. newspaper4k runs with `fetch_images=False` (its default downloads page images over the network "
          "during parsing; every tool here reads the same local snapshot).")
    speed_table("a2", web + ["pagesift-serve", "pagesift-081", "pagesift-081-serve"], [1, 16], "main-content extraction on the 269 A2 pages",
                "`pagesift` is the CLI run once per page (as in v1), so its per-item time includes process start-up; "
                "`pagesift serve` is the resident HTTP server (`POST /v2/parse`), like the other tools' resident workers.")

    # 2. Fetching
    crawl = ["pagesift-crawl", "pagesift-080-crawl", "pagesift-serve-crawl", "spider", "scrapy", "crawlee"]
    print("\n## 2. Fetching (crawler tools on a local replay of the A2 snapshots)\n")
    print("| Tool | Version | Pages | Completion ↑ | Failures ↓ | Timeouts ↓ | Pages with mojibake ↓ |")
    print("|---|---|---|---|---|---|---|")
    for t in crawl:
        r = a2["tools"].get(t) if a2 else None
        if not r:
            continue
        f, to = fails(r)
        garble = "n/a" if t == "crawlee" else str(r.get("garble_pages", "—"))
        print("| " + " | ".join([name(t), version("a2", t), str(r["items"]), f3(r["completion"]), str(f), str(to), garble]) + " |")
    print("\npagesift refuses pages it identifies as access barriers (captcha / challenge / block pages) instead of returning "
          "the barrier as content; that counts as a failure here. Crawlee's `HttpCrawler` does not decode bodies, so its "
          "mojibake count would measure the adapter, not Crawlee (n/a).")
    speed_table("a2", crawl + ["pagesift-081-crawl", "pagesift-081-serve-crawl"], [1, 16], "fetching the 269 A2 pages from the replay server",
                "The replay server is a small Python `ThreadingHTTPServer`; at 16 workers it can be the bottleneck for the "
                "fastest tools.")

    # 3. PDF
    b1, b2, b3 = q("b1"), q("b2"), q("b3")
    pdf = ["pagesift-file", "pagesift-080-file", "pdf_oxide", "pymupdf4llm", "kreuzberg", "mineru-flash"]
    models = ["mineru", "docling", "docling-gpu"]
    print("\n## 3. PDF\n")
    print("**olmOCR-bench, born-digital subsets (B1, 1,269 pages), score with failures = 0**\n")
    print("| Tool | Version | Pages | Completion ↑ | olmOCR score ↑ (official, incl. baseline) | Without baseline ↑ | Failures ↓ | Timeouts ↓ |")
    print("|---|---|---|---|---|---|---|---|")
    for t in pdf:
        r = b1["tools"].get(t)
        if not r:
            continue
        f, to = fails(r)
        print("| " + " | ".join([name(t), version("b1", t), str(r["items"]), f3(r["completion"]), f3(score(b1, "all", "olmOCR", t)),
                                 f3(score(b1, "all", "olmOCR 去掉 baseline", t)), str(f), str(to)]) + " |")
    print("\nMinerU's flash tier was stopped after 599 of 1,269 pages: at 10 parallel workers its memory (1.3–1.7 GB per "
          "worker) exhausted the machine and the kernel killed workers. Pages it did not reach count as not finished "
          "(v1 rules §4.2); on the 598 pages every tool finished it scores "
          f"{f3(score(b1, 'all', 'olmOCR', 'mineru-flash', 'common_only'))} vs pagesift "
          f"{f3(score(b1, 'all', 'olmOCR', 'pagesift-file', 'common_only'))}.\n")
    print("**Same, on the fixed 10 % model subset (144 pages), with the layout-model tools**\n")
    print("| Tool | olmOCR score ↑ (official) | Without baseline ↑ | Common items only ↑ (n) |")
    print("|---|---|---|---|")
    for t in pdf + models:
        if score(b1, "model_subset", "olmOCR", t) is None:
            continue
        print("| " + " | ".join([name(t), f3(score(b1, "model_subset", "olmOCR", t)), f3(score(b1, "model_subset", "olmOCR 去掉 baseline", t)),
                                 f"{f3(score(b1, 'model_subset', 'olmOCR', t, 'common_only'))} ({score(b1, 'model_subset', 'olmOCR', t, 'n_common')})"]) + " |")
    print("\nMarker 2.0.0 is not in the table: its default install cannot run on this machine (it needs a recent NVIDIA "
          "GPU; details in `v2/tools.lock.md`).\n")
    print("**Random real PDFs (B2: 71 arXiv / GOV.UK files) and hard PDFs (B3: 100 pdf.js test files)**\n")
    print("| Tool | B2 text completeness ↑ | B2 failures ↓ | B3 completion ↑ | B3 failures ↓ |")
    print("|---|---|---|---|---|")
    for t in pdf + models:
        r2, r3 = b2["tools"].get(t), b3["tools"].get(t)
        if not r2 and not r3:
            continue
        sub_ = t in models
        g = "model_subset" if sub_ else "all"
        print("| " + " | ".join([name(t) + (" (10 % subset)" if sub_ else ""), f3(score(b2, g, "B2", t)),
                                 str(sum(fails(r2))) if r2 else "—", f3(r3["completion"]) if r3 else "—",
                                 str(sum(fails(r3))) if r3 else "—"]) + " |")
    print("\nB2 text completeness: share of the words of the PDF's text layer (read by poppler `pdftotext`, not a competitor) "
          "that appear in the output. The model tools ran only the fixed 10 % subset (5 B2 files, 8 B3 files).")
    speed_table("b1", ["pagesift-file", "pagesift-081-file", "pagesift-serve-file", "pagesift-081-serve-file", "pagesift-080-file", "pdf_oxide", "pymupdf4llm", "kreuzberg", "mineru-flash"] + models,
                [1, 16], "PDF, same 144 pages (B1 10 % subset) for every tool",
                "Workers are admitted by available memory, so the memory-hungry tools run fewer than 16 (last column).")

    # 4. Office
    b4 = q("b4")
    off = ["pagesift-file", "pagesift-080-file", "kreuzberg", "markitdown", "mineru-flash", "pandoc", "extractous"]
    print("\n## 4. Office documents and e-books (B4, 81 files)\n")
    print("| Tool | Version | Files | Structure kept ↑ dev | Structure kept ↑ holdout | Completion ↑ | Failures ↓ |")
    print("|---|---|---|---|---|---|---|")
    for t in off:
        r = b4["tools"].get(t)
        if not r:
            continue
        print("| " + " | ".join([name(t), version("b4", t), str(r["items"]), f3(score(b4, "all", "B4 dev", t)),
                                 f3(score(b4, "all", "B4 holdout", t)), f3(r["completion"]), str(sum(fails(r)))]) + " |")
    print("\nDocling on the fixed 10 % subset (7 files): structure kept "
          f"{f3(score(b4, 'model_subset', 'B4 dev', 'docling'))} dev / {f3(score(b4, 'model_subset', 'B4 holdout', 'docling'))} holdout "
          f"(pagesift on the same files {f3(score(b4, 'model_subset', 'B4 dev', 'pagesift-file'))} / "
          f"{f3(score(b4, 'model_subset', 'B4 holdout', 'pagesift-file'))}). MinerU parses Office/EPUB/CSV only at its flash "
          "tier. extractous outputs plain text only. Six EPUB inputs were re-packed from the same IDPF commit (same content, "
          "different zip timestamps; `v2/samples/b4-repacked.tsv`).")
    speed_table("b4", ["pagesift-file", "pagesift-081-file", "pagesift-serve-file", "pagesift-081-serve-file", "pagesift-080-file", "kreuzberg", "markitdown", "mineru-flash", "pandoc", "extractous", "docling"],
                [1, 16], "Office and e-books, 81 files (Docling: its 7-file subset)")


    # 5. Scanned PDFs (B5)
    b5 = q("b5")
    if b5:
        print("\n## 5. Scanned PDFs (B5: olmOCR-bench old_scans + old_scans_math, all 134 pages)\n")
        print("| Tool | Pages | Completion ↑ | olmOCR score ↑ (official) | Without baseline ↑ |")
        print("|---|---|---|---|---|")
        for t in ["pagesift-file", "pdf_oxide", "pymupdf4llm", "kreuzberg", "docling-gpu", "mineru"]:
            r = b5["tools"].get(t)
            if not r:
                continue
            g = "model_subset" if t in ("docling-gpu", "mineru") else "all"
            print("| " + " | ".join([name(t), str(r["items"]), f3(r["completion"]), f3(score(b5, g, "olmOCR", t)),
                                     f3(score(b5, g, "olmOCR 去掉 baseline", t))]) + " |")
        print("\npagesift and the other text-layer tools do no OCR; on these scans only 15 of 134 pages have a text layer. "
              "Docling ran on the GPU build only (its CPU and GPU builds scored the same on B1). MinerU's default tier "
              "needs about 25 s per scanned page here and was stopped after 23 of 134 pages; the rest count as not finished.")

    # 6. JS-rendered pages (A3)
    a3 = RES / "quality-a3.md"
    if a3.exists():
        print("\n## 6. Pages that need JavaScript (A3, 100 live pages)\n")
        lines = a3.read_text(encoding="utf-8").splitlines()
        tbl = [l for l in lines if l.startswith("|")]
        print("| Tool | Completion ↑ | Reference-word coverage ↑ (all, unfinished = 0) | Coverage ↑ (pages both finished) | p50 ms ↓ | p95 ms ↓ | Peak MB ↓ |")
        print("|---|---|---|---|---|---|---|")
        for l in tbl[2:]:
            cells = l.strip("|").split("|")
            cells[0] = " " + name(cells[0].strip().replace("pagesift-crawl", "pagesift")) + " "
            print("|" + "|".join(cells) + "|")
        print("\nLive fetch, both tools in the same half hour, 4 workers each, while MinerU (B5) was still running on the "
              "machine. Reference text: Firefox (Playwright, not a competitor) rendered every page before and after the "
              "tools; only words present in both renders count. pagesift keeps the main content (default "
              "`onlyMainContent`), Crawl4AI returns whole-page Markdown, while the reference is the whole page's text; that "
              "is why Crawl4AI covers more on pages both finished. Firecrawl (self-hosted, official images) could not be "
              "deployed on this machine: its RabbitMQ container cannot read its Erlang cookie (`eacces`), see "
              "`v2/NOTES.md`.")


if __name__ == "__main__":
    main()

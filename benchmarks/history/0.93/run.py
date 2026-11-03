"""Run the 0.93 relocation cases through pagesift (`bench_relocation` example) and Scrapling's
`adaptive` mode on the same data; write results/summary.json.

Metrics per tool, per split and per change kind:
- correct rate ↑: the tool returned an element whose text is the expected text;
- wrong rate ↓: the tool returned an element with some other text (silently wrong);
- abstain rate: the tool returned nothing (pagesift: missing/ambiguous);
- per-field time ↓ (median, p95, µs): parse both pages, remember the element, find it again.

    cargo build --profile release-fast -p pagesift-extract --example bench_relocation
    python -I run.py [path/to/bench_relocation]
"""

import json
import pathlib
import re
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[3]
DATA = ROOT / "benchmarks" / "data" / "0.93"
OUT = pathlib.Path(__file__).parent / "results"


def key(t):
    return re.sub(r"\s+", "", t or "")


def run_pagesift(binary, cases):
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
        for c in cases:
            f.write(json.dumps({k: c[k] for k in ("id", "original_html", "selector", "mutated_html")}) + "\n")
        path = f.name
    out = subprocess.run([binary, path], capture_output=True, text=True, check=True).stdout
    res = {}
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 5:
            res[parts[0]] = (parts[1] == "true", parts[2], int(parts[4]))
    return res


def run_scrapling(cases):
    from scrapling.parser import Selector

    res = {}
    with tempfile.TemporaryDirectory() as d:
        for i, c in enumerate(cases):
            store = f"{d}/s{i}.db"
            t0 = time.perf_counter()
            try:
                old = Selector(c["original_html"], url=c["url"], adaptive=True, storage_args={"storage_file": store, "url": c["url"]})
                old.css(c["selector"], auto_save=True)
                new = Selector(c["mutated_html"], url=c["url"], adaptive=True, storage_args={"storage_file": store, "url": c["url"]})
                hits = new.css(c["selector"], adaptive=True)
                text = hits[0]._root.text_content() if len(hits) else ""
                found = len(hits) > 0
            except Exception:
                found, text = False, ""
            res[c["id"]] = (found, text, int((time.perf_counter() - t0) * 1e6))
    return res


def score(cases, res):
    groups = {}
    for c in cases:
        found, text, us = res.get(c["id"], (False, "", 0))
        verdict = "abstain" if not found else ("correct" if key(text) == key(c["expected"]) else "wrong")
        for g in ((c["split"], "all"), (c["split"], c["kind"])):
            groups.setdefault(g, []).append((verdict, us))
    out = {}
    for (split, kind), rows in sorted(groups.items()):
        n = len(rows)
        times = sorted(us for _, us in rows)
        out.setdefault(split, {})[kind] = {
            "n": n,
            "correct": round(sum(v == "correct" for v, _ in rows) / n, 4),
            "wrong": round(sum(v == "wrong" for v, _ in rows) / n, 4),
            "abstain": round(sum(v == "abstain" for v, _ in rows) / n, 4),
            "median_us": int(statistics.median(times)),
            "p95_us": times[min(n - 1, int(0.95 * n))],
        }
    return out


if __name__ == "__main__":
    binary = sys.argv[1] if len(sys.argv) > 1 else str(ROOT / "target" / "release-fast" / "examples" / "bench_relocation")
    cases = [json.loads(l) for l in open(DATA / "cases.jsonl")]
    summary = {
        "cases": len(cases),
        "pagesift": score(cases, run_pagesift(binary, cases)),
        "scrapling": score(cases, run_scrapling(cases)),
    }
    OUT.mkdir(exist_ok=True)
    (OUT / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for tool in ("pagesift", "scrapling"):
        h = summary[tool].get("holdout", {}).get("all", {})
        print(tool, "holdout", h)

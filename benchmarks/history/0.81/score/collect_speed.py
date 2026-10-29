#!/usr/bin/env python3
"""Turn the v2 speed run into metrics.tsv lines.

$RUN/v2speed/w<N>/<sub>/<tool>/{results.jsonl,summary.json}
"""
import json
import statistics
import sys
from pathlib import Path

root = Path(sys.argv[1])
GROUPS = {
    "a2": ("pagesift-081", "pagesift-081-serve"),
    "a2crawl": ("pagesift-081-crawl", "pagesift-081-serve-crawl"),
    "b1": ("pagesift-081-file", "pagesift-081-serve-file"),
    "b4": ("pagesift-081-file", "pagesift-081-serve-file"),
}
KIND = {"pagesift-081": "cli", "pagesift-081-crawl": "cli", "pagesift-081-file": "cli",
        "pagesift-081-serve": "serve", "pagesift-081-serve-crawl": "serve", "pagesift-081-serve-file": "serve"}


def read(sub, tool, w):
    d = root / f"w{w}" / sub / tool
    try:
        rows = [json.loads(line) for line in (d / "results.jsonl").read_text().splitlines() if line.strip()]
        summ = json.loads((d / "summary.json").read_text())
    except (OSError, ValueError):
        return None
    ms = sorted(r["ms"] for r in rows if r.get("status") == "ok" and r.get("ms") is not None)
    if not ms:
        return None
    peaks = [r.get("peak_mb") or 0 for r in rows]
    return {
        "p50": statistics.median(ms),
        "p95": ms[min(len(ms) - 1, int(0.95 * len(ms)))],
        "n": len(rows),
        "ok": sum(r.get("status") == "ok" for r in rows),
        "tput": len(rows) / summ["wall_s"] if summ.get("wall_s") else None,
        "peak": max(peaks) if peaks else None,
        "live": summ.get("peak_live_workers"),
    }


def f(x):
    return "" if x is None else f"{x:.6f}"


for group, tools in GROUPS.items():
    sub = "a2" if group == "a2crawl" else group
    for tool in tools:
        kind = KIND[tool]
        s1 = read(sub, tool, 1)
        s16 = read(sub, tool, 16)
        if s1:
            for k in ("p50", "p95", "tput", "peak", "live", "n", "ok"):
                print(f"v2.{group}.{kind}.1w.{k}\t{f(s1[k])}")
        if s16:
            for k in ("p50", "p95", "tput", "peak", "live", "n", "ok"):
                print(f"v2.{group}.{kind}.16w.{k}\t{f(s16[k])}")

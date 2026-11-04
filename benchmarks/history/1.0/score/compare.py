#!/usr/bin/env python3
"""Compare two metrics.tsv files (0.91.1 and the final build).

    python3 -I compare.py <old metrics.tsv> <new metrics.tsv> [--limit 0.10]

Prints every metric that got worse by more than the limit (speed, memory) or at all (quality).
"""
import argparse
import re
import tomllib
from pathlib import Path

DOWN = re.compile(r"(^|[._])(p50|p95|ms|peak|pss|mb|rss|wall|time|secs?|size|binary|cold|waste|leak|fail\w*|err\w*)([._]|$)")
UP = re.compile(r"(^|[._])(tput|score|f1|prec|rec|acc|recall|olmocr|nobase|completion|coverage|ok|aeb|wcxb|webmain|firecrawl|kept|pages_s|rate|completeness)([._]|$)")
QUALITY = re.compile(r"^quality\.|(^|[._])(f1|recall|olmocr|nobase|completion|coverage|score|kept)([._]|$)")


def load(path):
    out = {}
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        k, _, v = line.partition("\t")
        try:
            out[k] = float(v)
        except ValueError:
            pass
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("old")
    ap.add_argument("new")
    ap.add_argument("--limit", type=float, default=0.10)
    a = ap.parse_args()
    card = tomllib.loads((Path(__file__).parent / "scorecard.toml").read_text(encoding="utf-8"))
    direction = {r["psift"]: r["direction"] for r in card["row"] if isinstance(r.get("psift"), str)}
    old, new = load(a.old), load(a.new)
    unknown, worse, better, same = [], [], 0, 0
    print("| Metric | 0.91.1 | Final | Change |\n|---|---|---|---|")
    for k in sorted(set(old) & set(new)):
        d = direction.get(k) or ("down" if DOWN.search(k) else "up" if UP.search(k) else None)
        o, n = old[k], new[k]
        if d is None:
            unknown.append(k)
            continue
        change = (n - o) / o if o else (0.0 if n == o else float("inf"))
        loss = change if d == "down" else -change
        limit = 0.0005 if QUALITY.search(k) else a.limit
        if loss > limit:
            worse.append(k)
            print(f"| {k} {'↓' if d == 'down' else '↑'} | {o:g} | {n:g} | {change:+.1%} |")
        elif loss < -limit:
            better += 1
        else:
            same += 1
    print(f"\nWorse: {len(worse)}. Better: {better}. Within the limit: {same}.")
    print("No direction: " + (", ".join(unknown) or "none"))
    print("Only in 0.91.1: " + (", ".join(sorted(set(old) - set(new))) or "none"))
    print("Only in final: " + (", ".join(sorted(set(new) - set(old))) or "none"))


main()

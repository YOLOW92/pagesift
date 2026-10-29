#!/usr/bin/env python3
"""Turn the v2 quality JSON files into metrics.tsv lines (0.81 only)."""
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
TOOLS = ("pagesift-081", "pagesift-081-crawl", "pagesift-081-file")
PICK = {"a1": "pagesift-081", "a2": "pagesift-081", "a4": "pagesift-081",
        "b1": "pagesift-081-file", "b2": "pagesift-081-file", "b3": "pagesift-081-file",
        "b4": "pagesift-081-file", "b5": "pagesift-081-file"}


def f(x):
    return "" if x is None else f"{x:.6f}"


def score(d, group, set_, tool):
    try:
        return d["scores"][group][set_][tool]["all_zero_fail"]
    except (KeyError, TypeError):
        return None


for sub, tool in PICK.items():
    p = root / f"quality-{sub}.json"
    if not p.exists():
        print(f"# missing {p}", file=sys.stderr)
        continue
    d = json.loads(p.read_text())
    r = d.get("tools", {}).get(tool)
    if not r:
        print(f"# missing tool {tool} in {p}", file=sys.stderr)
        continue
    print(f"quality.{sub}.completion\t{f(r.get('completion'))}")
    if sub == "a2":
        c = d["tools"].get("pagesift-081-crawl") or {}
        print(f"quality.a2crawl.completion\t{f(c.get('completion'))}")
        print(f"quality.a2.garble_pages\t{r.get('garble_pages', '')}")
    for set_, key in (("AEB", "aeb"), ("WCXB", "wcxb"), ("WebMainBench", "webmain"), ("Firecrawl", "firecrawl")):
        v = score(d, "all", set_, tool)
        if v is not None:
            print(f"quality.a1.{key}\t{f(v)}")
    for set_, key in (("olmOCR", "olmocr"), ("olmOCR 去掉 baseline", "nobase")):
        v = score(d, "all", set_, tool)
        if v is not None:
            print(f"quality.{sub}.{key}\t{f(v)}")
    for set_, key in (("B2", "completeness"),):
        v = score(d, "all", set_, tool)
        if v is not None:
            print(f"quality.{sub}.{key}\t{f(v)}")
    for set_, key in (("B4 dev", "dev"), ("B4 holdout", "holdout")):
        v = score(d, "all", set_, tool)
        if v is not None:
            print(f"quality.{sub}.{key}\t{f(v)}")

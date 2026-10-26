#!/usr/bin/env python3
"""Scrapinghub article-extraction-benchmark (AEB) with fresh runs of every tool.

    prepare <aeb-repo> <out>             decompress html/*.html.gz to <out>/html, write <out>/manifest.jsonl
                                         ({id, path, url}) in the format the bench/compare adapters read
    score <aeb-repo> <out> <tool.jsonl>...   turn each adapter output ({id, markdown, ms}) into AEB's
                                         prediction format (Markdown stripped to plain text) and run the
                                         repository's own evaluate.py on a clean copy, so only these
                                         fresh outputs are scored (the outputs shipped with AEB are ignored)

Only the Python standard library is used.
"""

from __future__ import annotations

import gzip
import json
import re
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

IMAGE = re.compile(r"!\[[^\]]*\]\([^)]*\)")
LINK = re.compile(r"\[([^\]]*)\]\([^)]*\)")
MARKS = re.compile(r"^\s{0,3}(#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+)|[*_`|]+", re.M)


def plain(md: str) -> str:
    """Markdown → text: images removed, links reduced to their text, emphasis, headings, quotes, list markers and
    table pipes removed. Plain-text outputs (jusText, inscriptis, …) pass through unchanged."""
    # images go first (also the ones nested in a link: [![alt](src)](href)), then links keep their text
    text = IMAGE.sub("", md)
    prev = None
    while prev != text:
        prev, text = text, LINK.sub(lambda m: m.group(1), text)
    # 反斜杠转义（\* \_ \# …）还原成字符本身，转义正确的输出不该因此被扣分
    text = re.sub(r"\\([!-/:-@\[-`{-~])", r"\1", text)
    text = MARKS.sub("", text)
    return "\n".join(line.strip() for line in text.splitlines() if line.strip() and set(line.strip()) - set("-:= "))


def prepare(repo: Path, out: Path) -> None:
    truth = json.loads((repo / "ground-truth.json").read_text())
    (out / "html").mkdir(parents=True, exist_ok=True)
    with (out / "manifest.jsonl").open("w") as m:
        for key, row in truth.items():
            html = gzip.decompress((repo / "html" / f"{key}.html.gz").read_bytes())
            path = out / "html" / f"{key}.html"
            path.write_bytes(html)
            m.write(json.dumps({"id": key, "path": str(path.resolve()), "url": row["url"]}) + "\n")
    print(f"{len(truth)} pages in {out}", file=sys.stderr)


def score(repo: Path, out: Path, outputs: list[Path]) -> None:
    work = out / "eval"
    shutil.rmtree(work, ignore_errors=True)
    (work / "output").mkdir(parents=True)
    shutil.copy(repo / "evaluate.py", work)
    shutil.copy(repo / "ground-truth.json", work)
    keys = json.loads((repo / "ground-truth.json").read_text()).keys()
    timing = {}
    for f in outputs:
        rows = {}
        for line in f.open():
            if line.strip():
                r = json.loads(line)
                rows[r["id"]] = r
        name = f.stem
        pred = {k: {"articleBody": plain(rows.get(k, {}).get("markdown") or "")} for k in keys}
        (work / "output" / f"{name}.json").write_text(json.dumps({"version": "", "output": pred}, ensure_ascii=False))
        ms = [rows[k]["ms"] for k in keys if k in rows and "ms" in rows[k]]
        if ms:
            timing[name] = (statistics.median(ms), sorted(ms)[int(len(ms) * 0.95)])
    res = subprocess.run([sys.executable, "-I", "evaluate.py", "--n-bootstrap", "200", "--output", "results.json"],
                         cwd=work, capture_output=True, text=True)
    sys.stdout.write(res.stdout)
    if res.returncode:
        sys.stderr.write(res.stderr)
        raise SystemExit(res.returncode)
    results = json.loads((work / "results.json").read_text())
    print("\n| Tool | F1 ↑ | Precision ↑ | Recall ↑ | Accuracy ↑ | p50 ms ↓ | p95 ms ↓ |\n|---|---|---|---|---|---|---|")
    for name, m in sorted(results.items(), key=lambda kv: -kv[1]["f1"]):
        p50, p95 = timing.get(name, (float("nan"), float("nan")))
        print(f"| {name} | {m['f1']:.3f} | {m['precision']:.3f} | {m['recall']:.3f} | {m['accuracy']:.3f} | {p50:.2f} | {p95:.1f} |")


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "prepare":
        prepare(Path(sys.argv[2]), Path(sys.argv[3]))
    elif cmd == "score":
        score(Path(sys.argv[2]), Path(sys.argv[3]), [Path(p) for p in sys.argv[4:]])
    else:
        raise SystemExit(__doc__)

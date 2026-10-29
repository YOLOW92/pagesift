#!/usr/bin/env python3
"""Archive SHA-256 of every produced output file.

    hash_outputs.py <root> <out-dir> <label>

Writes <out-dir>/<label>.tsv (relative path, bytes, sha256) and prints a
one-line summary for metrics.tsv.  This is the "output hash archive" the
0.81 round uses instead of a byte-for-byte 0.80.10 comparison.
"""
import hashlib
import sys
from pathlib import Path

root, outdir, label = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
outdir.mkdir(parents=True, exist_ok=True)
dest = outdir / f"{label}.tsv"
EXTS = {".md", ".jsonl", ".json", ".txt", ".tsv", ".html"}
rows = []
for p in sorted(root.rglob("*")):
    if not p.is_file() or p.suffix.lower() not in EXTS:
        continue
    if p.name in ("worker.log",):
        continue
    b = p.read_bytes()
    rows.append((p.relative_to(root).as_posix(), len(b), hashlib.sha256(b).hexdigest()))
with open(dest, "w", encoding="utf-8") as f:
    f.write("path\tbytes\tsha256\n")
    for r in rows:
        f.write(f"{r[0]}\t{r[1]}\t{r[2]}\n")
total = sum(r[1] for r in rows)
print(f"hashes.{label}.files\t{len(rows)}")
print(f"hashes.{label}.bytes\t{total}")
print(f"# {dest}", file=sys.stderr)

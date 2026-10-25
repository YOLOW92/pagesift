#!/usr/bin/env python3
"""E1-A 混合语料构建：把四项基准的原始样本合成一个交错流。

- 读 WebMainBench / WCEB / WCXB 的原始 HTML，按内容 BLAKE3/SHA256 去重后交错写入；
  同一份内容只出现一次，不把重复样本算成新增样本。
- 输出 manifest.jsonl: {track_id, source, html, url}
- 同时输出 corpus_stats.json: 各来源条数、去重丢弃数、总字节。
"""
import hashlib, json, random, sys
from pathlib import Path

def h(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()

def wmb(root: Path, seen: set):
    p = root / "bench/data/p4_2_webmainbench/webmainbench.jsonl"
    for line in p.open(encoding="utf-8"):
        if not line.strip():
            continue
        r = json.loads(line)
        html = r.get("html") or ""
        if not html:
            continue
        k = h(html.encode("utf-8", "replace"))
        if k in seen:
            continue
        seen.add(k)
        yield {"track_id": "wmb-" + r["track_id"], "source": "webmainbench",
               "url": r.get("url") or "", "html": html}

def wceb(root: Path, seen: set):
    base = root / "bench/upstream/WCEB/datasets/combined/html"
    for ds in sorted(p for p in base.iterdir() if p.is_dir()):
        for f in sorted(ds.glob("*.html")):
            data = f.read_bytes()
            k = h(data)
            if k in seen:
                continue
            seen.add(k)
            yield {"track_id": f"wceb-{ds.name}-{f.stem}", "source": "wceb",
                   "url": "", "html": data.decode("utf-8", "replace")}

def wcxb(root: Path, seen: set):
    base = root / "bench/data/wcxb"
    for split in ("dev", "test"):
        d = base / split / "html"
        if not d.is_dir():
            continue
        for f in sorted(d.glob("*.html")):
            data = f.read_bytes()
            k = h(data)
            if k in seen:
                continue
            seen.add(k)
            yield {"track_id": f"wcxb-{split}-{f.stem}", "source": "wcxb",
                   "url": "", "html": data.decode("utf-8", "replace")}

def main():
    root = Path(sys.argv[1]); out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    seen = set()
    streams = [list(wmb(root, seen)), list(wceb(root, seen)), list(wcxb(root, seen))]
    counts = {s[0]["source"] if s else "empty": len(s) for s in streams}
    # 交错：轮转取各来源，保证早期就混合，而不是按来源分块
    random.seed(20261025)
    iters = [iter(s) for s in streams]
    alive = [True] * len(streams)
    total = 0
    bytes_total = 0
    with (out / "manifest.jsonl").open("w", encoding="utf-8") as f:
        while any(alive):
            for i, it in enumerate(iters):
                if not alive[i]:
                    continue
                try:
                    row = next(it)
                except StopIteration:
                    alive[i] = False
                    continue
                f.write(json.dumps(row, ensure_ascii=False) + "\n")
                total += 1
                bytes_total += len(row["html"])
    stats = {"sources": counts, "total": total, "html_bytes": bytes_total,
             "deduped_removed": "见 sources 与各自原始条数之差"}
    (out / "corpus_stats.json").write_text(json.dumps(stats, indent=2), encoding="utf-8")
    print(json.dumps(stats, indent=2))

if __name__ == "__main__":
    main()

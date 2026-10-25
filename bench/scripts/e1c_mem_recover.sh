#!/bin/bash
# E1-C 补充：内存约束下的回落 + 进程中断后的恢复与去重复核
cd ~/pagesift
OUT=bench/results/final/0.70/e1/C2
CFG=bench/scripts/e1.toml

echo "===== 内存约束：256 MiB 地址空间上限 ====="
rm -rf "$OUT/mem256"; ( ulimit -v 262144
  ./target/release/pagesift --config "$CFG" crawl "http://127.0.0.1:8110/" \
    --out "$OUT/mem256" --limit 400 --depth 8 --entire-domain --engine http 2>&1 | tail -4
) ; echo "mem256_exit=$?"
echo "  pages=$(ls "$OUT/mem256" 2>/dev/null | wc -l)"

echo "===== 中断恢复：跑到一半 kill，再续跑 ====="
rm -rf "$OUT/rec1" "$OUT/rec2"
./target/release/pagesift --config "$CFG" crawl "http://127.0.0.1:8110/" \
  --out "$OUT/rec1" --limit 400 --depth 8 --entire-domain --engine http > "$OUT/rec1.log" 2>&1 &
P=$!
sleep 1.0
kill -9 $P 2>/dev/null
wait $P 2>/dev/null
echo "  第一次被中断，pages=$(ls "$OUT/rec1" 2>/dev/null | wc -l)"
./target/release/pagesift --config "$CFG" crawl "http://127.0.0.1:8110/" \
  --out "$OUT/rec2" --limit 400 --depth 8 --entire-domain --engine http > "$OUT/rec2.log" 2>&1
echo "  续跑完成，pages=$(ls "$OUT/rec2" 2>/dev/null | wc -l)"

python3 - <<'PY'
import json, hashlib
from pathlib import Path
def load(d):
    p = Path(d) / "index.jsonl"
    urls, hashes = [], set()
    if not p.exists(): return urls, hashes
    for line in p.open(encoding="utf-8"):
        if not line.strip(): continue
        r = json.loads(line)
        urls.append(r["url"])
        f = Path(d) / r["file"]
        if f.exists():
            hashes.add(hashlib.sha256(f.read_bytes()).hexdigest())
    return urls, hashes
root = Path("/home/yolow92/pagesift/bench/results/final/0.70/e1/C2")
u1, h1 = load(root / "rec1")
u2, h2 = load(root / "rec2")
full, hf = load(root / "h1")
print(f"  中断轮 unique_url={len(set(u1))}  续跑 unique_url={len(set(u2))}  完整对照={len(set(full))}")
print(f"  续跑是否覆盖完整集: {set(full) <= set(u2)}")
print(f"  续跑内是否有重复 URL: {len(u2) - len(set(u2))}")
print(f"  续跑 unique_content={len(h2)} vs 完整 {len(hf)}")
PY
echo E1C_SUPP_DONE

#!/usr/bin/env python3
"""中断恢复核查：把被 kill 的爬取与续跑的输出合起来，看是否有重复与空洞。"""
import hashlib, json, sys
from pathlib import Path

root = Path(sys.argv[1])
def load(d):
    idx = d / "index.jsonl"
    urls, hashes = [], set()
    if not idx.exists():
        return urls, hashes
    for line in idx.open(encoding="utf-8"):
        if not line.strip():
            continue
        r = json.loads(line)
        urls.append(r.get("url", ""))
        p = d / r.get("file", "")
        if p.exists():
            hashes.add(hashlib.sha256(p.read_bytes()).hexdigest())
    return urls, hashes

a, ha = load(root / "b_h2")          # 被中断
b, hb = load(root / "b_h2_resumed")  # 续跑
ref = Path(sys.argv[2])              # 同主机的完整参照（E1-B 的 h2）
full, hf = load(ref)

ua, ub = set(a), set(b)
res = {
    "interrupted_run_outputs": len(a),
    "resumed_run_outputs": len(b),
    "reference_complete_run": len(full),
    "interrupted_unique_url": len(ua),
    "resumed_unique_url": len(ub),
    "overlap_urls": len(ua & ub),
    "union_urls": len(ua | ub),
    "reference_unique_url": len(set(full)),
    "duplicate_outputs_across_runs": len(a) + len(b) - len(ua | ub),
    "holes_vs_reference": sorted(set(full) - (ua | ub))[:10],
    "hole_count": len(set(full) - (ua | ub)),
    "unique_content_union": len(ha | hb),
    "unique_content_reference": len(hf),
}
print(json.dumps(res, ensure_ascii=False, indent=2))
(root / "recovery.json").write_text(json.dumps(res, ensure_ascii=False, indent=2), encoding="utf-8")

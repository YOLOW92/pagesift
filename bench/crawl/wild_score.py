"""汇总 run_wild.sh 的结果：每个场景、每个工具一行。

python3 -I wild_score.py <结果目录> [每站上限，默认 60]
覆盖率 = 抓到的独立内容页 / min(每站内容页, 上限)，越高越好；
浪费率 = 不是"首次抓到的内容页或导航页"的请求占比（重复、陷阱、二进制、robots 违规、404），越低越好；
robots 违规、429 次数越低越好；503 是源站故意的首发失败，重试成功就不影响覆盖率。
"""
import json, os, sys

def main():
    d = sys.argv[1]
    limit = int(sys.argv[2]) if len(sys.argv) > 2 else 60
    meas = {}
    path = os.path.join(d, "measure.jsonl")
    if os.path.exists(path):
        for line in open(path):
            r = json.loads(line)
            meas[r["label"]] = r
    rows = []
    for f in sorted(os.listdir(d)):
        if not f.endswith(".json"):
            continue
        scen, tool = f[:-5].split("-", 1)
        try:
            sites = json.load(open(os.path.join(d, f)))
        except json.JSONDecodeError:
            continue
        req = sum(s["requests"] for s in sites)
        content = sum(s["unique_content"] for s in sites)
        kinds = {}
        for s in sites:
            for k, v in s["kinds"].items():
                kinds[k] = kinds.get(k, 0) + v
        useful = kinds.get("content", 0) + kinds.get("nav", 0)
        waste = req - useful - kinds.get("503", 0) - kinds.get("429", 0)
        m = meas.get(f"wild-{scen}/{tool}", {})
        rows.append((scen, tool, content / (len(sites) * limit), waste / max(req, 1), req,
                     kinds.get("robots", 0), kinds.get("429", 0), m.get("wall_s"), m.get("peak_mb") or m.get("peak_rss_mb"), kinds))
    print(f"{'场景':8} {'工具':14} {'覆盖率↑':>8} {'浪费率↓':>8} {'请求数':>7} {'robots违规↓':>11} {'429↓':>6} {'耗时s↓':>8}")
    for r in rows:
        print(f"{r[0]:8} {r[1]:14} {r[2]:8.3f} {r[3]:8.3f} {r[4]:7d} {r[5]:11d} {r[6]:6d} {str(r[7]):>8}")
    print()
    for r in rows:
        print(r[0], r[1], json.dumps(r[9], sort_keys=True))

main()

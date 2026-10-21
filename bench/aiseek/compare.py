"""Side-by-side summary of bench.py results for Crawl4AI and pagesift (results/crawl4ai and results/pagesift).

Every metric is labelled with its direction. Usage: python3 compare.py
"""

from __future__ import annotations

import collections
import json
from pathlib import Path

RES = Path(__file__).resolve().parent / "results"
SIDES = ("crawl4ai", "pagesift")


def q(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))] if xs else float("nan")


def load(side, name):
    f = RES / side / f"{name}.jsonl"
    return [json.loads(l) for l in open(f)] if f.exists() else None


def res(side, name):
    s = load(side, f"{name}-res") or []
    if len(s) < 2:
        return {}
    mem = [x["mem"] / 2**20 for x in s]
    cpu = (s[-1]["cpu_us"] - s[0]["cpu_us"]) / 1e6
    wall = s[-1]["t"] - s[0]["t"]
    return {"mem_p50": q(mem, 0.5), "mem_max": max(mem), "cores": cpu / wall, "browsers_max": max(x["chrome"] for x in s)}


def ok(r):
    return r.get("http") == 200 and (r.get("status") or 0) < 400


def table(title, rows):
    print(f"\n### {title}\n")
    print("| 指标 | 方向 | Crawl4AI | pagesift |")
    print("|---|---|---|---|")
    for name, direction, a, b in rows:
        print(f"| {name} | {direction} | {a} | {b} |")


def fmt(x, nd=2):
    return "—" if x is None or x != x else f"{x:.{nd}f}"


def pipeline():
    d = {s: load(s, "pipeline") for s in SIDES}
    if not all(d.values()):
        return
    rows = []
    for name, f, direction, nd in [
        ("每篇总耗时 p50 (s)", lambda rs: q([r["total"] for r in rs], 0.5), "越低越好", 2),
        ("每篇总耗时 p95 (s)", lambda rs: q([r["total"] for r in rs], 0.95), "越低越好", 2),
        ("HTTP 步骤 p50 (s)", lambda rs: q([r["http_step"]["sec"] for r in rs], 0.5), "越低越好", 2),
        ("渲染步骤 p50 (s)", lambda rs: q([r["render_step"]["sec"] for r in rs if "render_step" in r], 0.5), "越低越好", 2),
        ("渲染步骤 p95 (s)", lambda rs: q([r["render_step"]["sec"] for r in rs if "render_step" in r], 0.95), "越低越好", 2),
        ("送去渲染的篇数", lambda rs: sum(1 for r in rs if "render_step" in r), "越低越好", 0),
        ("最终拿到正文的篇数", lambda rs: sum(1 for r in rs if final_ok(r)), "越高越好", 0),
    ]:
        rows.append((name, direction, fmt(f(d["crawl4ai"]), nd), fmt(f(d["pagesift"]), nd)))
    for k, direction in [("mem_p50", "越低越好"), ("mem_max", "越低越好"), ("cores", "越低越好")]:
        a, b = res("crawl4ai", "pipeline").get(k), res("pagesift", "pipeline").get(k)
        rows.append((label(k), direction, fmt(a, 0 if "mem" in k else 2), fmt(b, 0 if "mem" in k else 2)))
    table("pipeline：210 条生产网址，先 HTTP，正文 < 1200 字符再渲染（AiSeek 的调用逻辑）", rows)
    print("\n判定分布：", {s: dict(collections.Counter(r["verdict"] for r in d[s])) for s in SIDES})


def final_ok(r):
    if r["verdict"] == "use":
        return True
    rs = r.get("render_step")
    return bool(rs and ok(rs) and rs.get("chars", 0) >= 200)


def label(k):
    return {"mem_p50": "内存 p50 (MB)", "mem_max": "内存峰值 (MB)", "cores": "平均 CPU (核)", "browsers_max": "浏览器进程峰值"}[k]


def render():
    d = {s: load(s, "render") for s in SIDES}
    if not all(d.values()):
        return
    rows = []
    for name, f, direction, nd in [
        ("全部 p50 (s)", lambda rs: q([r["sec"] for r in rs], 0.5), "越低越好", 2),
        ("全部 p95 (s)", lambda rs: q([r["sec"] for r in rs], 0.95), "越低越好", 2),
        ("全部 p99 (s)", lambda rs: q([r["sec"] for r in rs], 0.99), "越低越好", 2),
        ("成功（状态 < 400）", lambda rs: sum(1 for r in rs if ok(r)), "越高越好", 0),
        ("成功且正文 ≥ 200 字符", lambda rs: sum(1 for r in rs if ok(r) and r.get("chars", 0) >= 200), "越高越好", 0),
        ("报 3xx 状态码（取了重定向第一跳）", lambda rs: sum(1 for r in rs if r.get("http") == 200 and 300 <= (r.get("status") or 0) < 400), "越低越好", 0),
        ("服务端报错（非 200）", lambda rs: sum(1 for r in rs if r.get("http") != 200), "越低越好", 0),
    ]:
        rows.append((name, direction, fmt(f(d["crawl4ai"]), nd), fmt(f(d["pagesift"]), nd)))
    for k, direction in [("mem_p50", "越低越好"), ("mem_max", "越低越好"), ("cores", "越低越好"), ("browsers_max", "越低越好")]:
        a, b = res("crawl4ai", "render").get(k), res("pagesift", "render").get(k)
        rows.append((label(k), direction, fmt(a, 0 if k != "cores" else 2), fmt(b, 0 if k != "cores" else 2)))
    table("render：同样 210 条，全部渲染", rows)
    for s in SIDES:
        errs = collections.Counter(f"{r.get('http')} {(r.get('err') or '')[:60]}" for r in d[s] if r.get("http") != 200)
        print(f"\n{s} 报错：", dict(errs.most_common(8)))
    # 同一网址两边都成功时的正文长度
    a = {r["url"]: r for r in d["crawl4ai"] if ok(r)}
    b = {r["url"]: r for r in d["pagesift"] if ok(r)}
    both = [(a[u]["chars"], b[u]["chars"]) for u in a.keys() & b.keys()]
    more = sum(1 for x, y in both if y > x * 1.2)
    less = sum(1 for x, y in both if y < x * 0.8)
    print(f"\n同一网址两边都成功：{len(both)} 条；pagesift 正文多 20% 以上 {more} 条，少 20% 以上 {less} 条，相近 {len(both) - more - less} 条")
    print("只有 Crawl4AI 成功：", len(a.keys() - b.keys()), "；只有 pagesift 成功：", len(b.keys() - a.keys()))


def sweep():
    rows = []
    for pages in (4, 8, 0):
        for s in SIDES:
            rs = load(s, f"sweep-p{pages}")
            if not rs:
                continue
            wall = rs[-1].get("wall")
            reqs = [r for r in rs if "sec" in r]
            r = res(s, f"sweep-p{pages}")
            rows.append((f"{'自适应' if pages == 0 else pages} 页", s, len(reqs) / wall if wall else None,
                         q([x["sec"] for x in reqs], 0.5), q([x["sec"] for x in reqs], 0.95),
                         sum(1 for x in reqs if ok(x)), len(reqs), r.get("mem_max"), r.get("cores")))
    if not rows:
        return
    print("\n### sweep：25 条固定网址循环 240 次，调用方 = 2 × 页数\n")
    print("| 并发页面 | 工具 | 吞吐 req/s（越高越好） | p50 s（越低越好） | p95 s（越低越好） | 成功（越高越好） | 内存峰值 MB（越低越好） | CPU 核（越低越好） |")
    print("|---|---|---|---|---|---|---|---|")
    for p, s, tput, p50, p95, good, n, mem, cores in rows:
        print(f"| {p} | {s} | {fmt(tput)} | {fmt(p50)} | {fmt(p95)} | {good}/{n} | {fmt(mem, 0)} | {fmt(cores)} |")


def cold():
    rows = []
    for s in SIDES:
        rs = load(s, "cold")
        if rs:
            rows.append((s, q([r["ready"] for r in rs], 0.5), q([r["first_render"] for r in rs], 0.5),
                         q([r["second_render"] for r in rs], 0.5), q([r["http"] for r in rs], 0.5)))
    if not rows:
        return
    print("\n### cold：冷启动（5 次的中位数，秒，越低越好）\n")
    print("| 工具 | 启动到可用 | 第一次渲染 | 第二次渲染 | 纯 HTTP |")
    print("|---|---|---|---|---|")
    for s, *v in rows:
        print(f"| {s} | " + " | ".join(fmt(x) for x in v) + " |")


if __name__ == "__main__":
    pipeline()
    render()
    sweep()
    cold()

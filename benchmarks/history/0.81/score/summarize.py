#!/usr/bin/env python3
"""Build the 0.81 two-tier view and the full detailed report.

    python3 -I summarize.py <run-dir> [--out <archive-dir>]

Reads <run-dir>/metrics.tsv (key<TAB>value, last value wins) and
score/scorecard.toml.  Writes report.md, headline-zh.md, headline-en.md,
scorecard.json and metrics.json into the archive directory (default:
benchmarks/history/0.81/results/<run name>).
"""
import argparse
import json
import math
import os
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
HIST = HERE.parent
ROOT = HIST.parent.parent
META = tomllib.loads((HERE / "scorecard.toml").read_text(encoding="utf-8"))


def read_metrics(path):
    out = {}
    if not path.exists():
        return out
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip() or "\t" not in line:
            continue
        k, v = line.split("\t", 1)
        out[k] = v
    return out


def num(x):
    try:
        v = float(x)
        return v if math.isfinite(v) else None
    except (TypeError, ValueError):
        return None


def unit_of(row):
    m = row["metric_en"]
    if "pages/s" in m:
        return "pages/s"
    if "files/s" in m:
        return "files/s"
    if "ms/page" in m:
        return "ms/page"
    if "ms/question" in m:
        return "ms/question"
    if "total ms" in m:
        return "ms"
    if "seconds" in m:
        return "s"
    if "peak memory" in m:
        return "MB"
    return ""


def fmt(v, row, digits=None):
    if v is None:
        return "—"
    unit = unit_of(row)
    if unit in ("pages/s", "files/s", "MB"):
        return f"{v:,.1f}"
    if unit in ("ms/page", "ms/question", "s"):
        return f"{v:.1f}" if v >= 1 else f"{v:.2f}"
    if unit == "ms":
        return f"{v:,.0f}"
    if row["kind"] == "quality":
        return f"{v:.3f}"
    return f"{v:.2f}" if digits is None else f"{v:.{digits}f}"


def classify(row, p, r):
    if not r:
        return ("n/a", None, None)
    if row.get("kind") == "memory":
        ratio = p / r if r else None
        return ("memory", ratio, None)
    if row.get("kind") == "speed":
        if row["direction"] == "up":
            ratio = p / r if r else None
        else:
            ratio = r / p if p else None
        if ratio is None:
            return ("n/a", None, None)
        if ratio >= META["meta"].get("far_speed", 1.75):
            return ("far", ratio, None)
        if ratio >= META["meta"].get("ahead_speed", 1.10):
            return ("ahead", ratio, None)
        if ratio >= 1.0:
            return ("parity", ratio, None)
        return ("behind", ratio, None)
    delta = (p - r) if row["direction"] == "up" else (r - p)
    if delta >= META["meta"].get("far_quality", 0.10):
        return ("far", None, delta)
    if delta >= META["meta"].get("ahead_quality", 0.01):
        return ("ahead", None, delta)
    if delta >= -0.01:
        return ("parity", None, delta)
    return ("behind", None, delta)


def strength(row, tier, ratio, delta):
    if tier in ("far", "ahead"):
        if ratio is not None:
            return ratio
        return 1.0 + (delta or 0)
    if row.get("kind") == "speed" and ratio is not None:
        return ratio
    return 1.0 + (delta or 0)


def comparison(row, ratio, delta):
    if ratio is not None:
        return f"×{ratio:.2f}"
    if delta is not None:
        return f"{delta:+.3f}"
    return "—"


def classify_label(tier):
    return {"far": "大幅领先", "ahead": "领先", "parity": "持平", "behind": "落后", "memory": "内存", "n/a": "无对手"}[tier]


def mem_cell(r):
    pm, rm = r.get("_pm"), r.get("_rm")
    if pm is None and rm is None:
        return "—"
    left = f"{pm:,.0f}" if pm is not None else "—"
    right = f"{rm:,.0f}" if rm is not None else "—"
    return f"{left} / {right}"


def table(rows, lang):
    if not rows:
        return "_没有满足条件的项目。_\n"
    show_mem = any(r.get("_pm") is not None or r.get("_rm") is not None for r in rows)
    if lang == "zh":
        head = ["领域", "工作负载", "指标", "PageSift 0.81", "最快对手", "对手", "差距"]
        if show_mem:
            head.append("峰值内存 MB（PageSift / 对手）")
    else:
        head = ["Domain", "Workload", "Metric", "PageSift 0.81", "Fastest rival", "Rival", "Gap"]
        if show_mem:
            head.append("Peak MB/worker (PageSift / rival)")
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    for r in rows:
        vals = r["_fmt"]
        cells = [
            r["domain_zh"] if lang == "zh" else r["domain_en"],
            r["workload_zh"] if lang == "zh" else r["workload_en"],
            r["metric_zh"] if lang == "zh" else r["metric_en"],
            vals[0], r["rival_name"], vals[1], vals[2],
        ]
        if show_mem:
            cells.append(mem_cell(r))
        lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


def gap_rows(rows):
    """Rows for the one-line gap sentence.

    Behind rows always win; if nothing is behind (the goal of the release),
    fall back to the three closest comparisons so the sentence still says
    something true instead of claiming a gap that is not there.
    """
    def severity(r):
        if r.get("kind") == "speed" and r["_ratio"] is not None:
            return r["_ratio"]
        return 1.0 + (r["_delta"] or 0.0)

    behind = sorted([r for r in rows if r["_tier"] == "behind"], key=severity)[:3]
    if behind:
        return behind, "behind"
    closest = sorted([r for r in rows if r["_tier"] in ("parity", "ahead", "far")], key=strength_key)
    return closest[:3], "closest"


def strength_key(r):
    return r["_ratio"] if r["_ratio"] is not None else 1.0 + (r["_delta"] or 0.0)


# A2 回放只有一个主机，吞吐差距主要来自按主机的默认间隔；写在差距句后面。
A2_RATE = {
    "zh": "A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。",
    "en": "A2 throughput is measured against a replay server on a single loopback address, so all 269 pages count as one host. PageSift waits 1 s between requests to the same host by default; spider has no such limit. This row mostly reflects that politeness interval, not engine speed.",
}


def gap_sentence(rows, lang="zh"):
    parts = []
    for r in rows:
        p, rv = r["_psift"], r["rival"]
        if p is None:
            continue
        workload = r["workload_zh"] if lang == "zh" else r["workload_en"]
        if r["kind"] == "speed":
            parts.append(f"{workload} {fmt(p, r)} vs {r['rival_name']} {fmt(rv, r)} {unit_of(r)}")
        else:
            parts.append(f"{workload} {fmt(p, r)} vs {r['rival_name']} {fmt(rv, r)}")
    if lang == "zh":
        return "；".join(parts) + "。完整表见 [报告](report.md)。" if parts else ""
    return "; ".join(parts) + ". Full table: [report](report.md)." if parts else ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run", type=Path)
    ap.add_argument("--out", type=Path)
    ap.add_argument("--json", action="store_true", help="also print the scorecard JSON")
    a = ap.parse_args()
    run = a.run
    out = a.out or (HIST / "results" / run.name)
    out.mkdir(parents=True, exist_ok=True)
    metrics = read_metrics(run / "metrics.tsv")

    rows = []
    missing = []
    for row in META["row"]:
        row = dict(row)
        p = num(metrics.get(row["psift"]))
        if p is None:
            missing.append(row["psift"])
            row["_psift"] = None
            row["_tier"] = "n/a"
            row["_ratio"] = None
            row["_delta"] = None
        else:
            tier, ratio, delta = classify(row, p, float(row["rival"]))
            row["_psift"] = p
            row["_tier"] = tier
            row["_ratio"] = ratio
            row["_delta"] = delta
        row["_pm"] = num(metrics.get(row["psift_mem"])) if row.get("psift_mem") else None
        row["_rm"] = float(row["rival_mem"]) if row.get("rival_mem") is not None else None
        row["_fmt"] = (fmt(row["_psift"], row), fmt(float(row["rival"]), row) if row["rival"] else "—",
                       comparison(row, row["_ratio"], row["_delta"]))
        rows.append(row)

    headline = [r for r in rows if r.get("headline", True) and not r.get("appendix") and r["_tier"] in ("far", "ahead")]
    far = sorted([r for r in headline if r["_tier"] == "far"], key=lambda r: -strength(r, r["_tier"], r["_ratio"], r["_delta"]))
    ahead = sorted([r for r in headline if r["_tier"] == "ahead"], key=lambda r: -strength(r, r["_tier"], r["_ratio"], r["_delta"]))
    gaps, gap_kind = gap_rows(rows)

    meta = META["meta"]
    header = (
        f"PageSift {meta['pagesift']} was re-measured on {meta['date']} on the same machine "
        f"({meta['machine']}). Competitor numbers are the recorded 2026-10-21/22/27/28 runs; "
        "they were **not re-run** in this round, so this is a 0.81 performance refresh, not a new head-to-head. "
        "Speed rows use the best PageSift configuration for that workload (CLI or resident serve); "
        "one row per workload. Memory is a column, not a separate headline.\n"
    )

    md = [f"# PageSift 0.81 performance refresh ({meta['date']})\n", header, "\n## 大幅领先\n", table(far, "zh"),
          "\n## 领先\n", table(ahead, "zh"), "\n## 最大差距\n", gap_sentence(gaps, "zh"), "\n", A2_RATE["zh"] + "\n",
          "## 完整对照\n",
          "每一行是一个工作负载 + 一个指标；PageSift 取该工作负载的最佳配置。"
          "`持平` 和 `落后` 不进入首屏，但全部列在下面。\n"]
    domains = []
    for r in rows:
        if r["domain_zh"] not in domains:
            domains.append(r["domain_zh"])
    for d in domains:
        rs = [r for r in rows if r["domain_zh"] == d]
        md.append(f"\n### {d}\n")
        show_mem = any(x.get("_pm") is not None or x.get("_rm") is not None for x in rs)
        head = "| 工作负载 | 指标 | PageSift 0.81 | 对手 | 对手值 | 对比 | 级别 |"
        sep = "|---|---|---:|---|---:|---:|---|"
        if show_mem:
            head += " 峰值内存 MB（PageSift / 对手） |"
            sep += "---|"
        md.append(head + " 对手数据 |")
        md.append(sep + "---|")
        for r in rs:
            cells = [r["workload_zh"], r["metric_zh"], r["_fmt"][0], r["rival_name"], r["_fmt"][1],
                     r["_fmt"][2], classify_label(r["_tier"])]
            if show_mem:
                cells.append(mem_cell(r))
            cells.append(f"{r['rival_source']} ({r['rival_date']})")
            md.append("| " + " | ".join(cells) + " |")
    md.append("\n## 方法与限制\n")
    md.append("- 输入、样本、评分规则沿用 `benchmarks/mix/v2/RULES.md` 和各领域 README；0.81 只跑 PageSift，竞品不重跑。")
    md.append("- " + A2_RATE["zh"])
    md.append("- A2 的 269 页快照在 2026-10-29 重新抓取，绝对耗时不能和 10-28 的旧 A2 直接比；B1/B4 输入不变。")
    md.append("- 每个输出文件的 SHA-256 存在 `hashes/`；这是“只跑 0.81”下的输出哈希存档，不是 0.80.10 的字节级对比。")
    md.append("- 浏览器、A3、10M scale、Deployment/API 是没有同口径竞品的附录项，不进入两级表。")
    md.append("- 转换质量与 0.80.9 的记录值一致（AEB 0.954、WCXB 0.866、WebMainBench 0.833、Firecrawl 0.480、B2 0.937、B3 0.640、B4 0.988、B5 0.112）；没有跑 0.80.10 做字节对比，输出 SHA-256 在 `hashes/`。")
    md.append("- A2 serve 1 路的 p50 首测 3.22 ms，随后在空机上重测 5 次，中位 3.06 ms（范围 2.21–3.79 ms），表中用重测中位；A2 快照是 10-29 重抓的，绝对耗时不要和 10-28 的旧 A2 直接比。")
    md.append("- 爬取两项低于旧记录：wide 2,176 页/秒（docs §6 记录 3,500–4,400），wild fast 覆盖 0.685（旧 0.770）、polite 0.740（旧 0.755）；机器和版本口径可能不同，但本轮按实测值报告，不把它们算作提升项。")
    md.append("- RAG 查询过滤的 1.7 ms/题是进程内筛选，不含每次加载语料；旧首页的 387.5 ms/题是另一种调用方式，两者不要直接比。")
    if missing:
        md.append("- **缺少指标**：" + ", ".join(f"`{m}`" for m in missing))
    (out / "report.md").write_text("\n".join(md) + "\n", encoding="utf-8")

    zh = [f"## 性能一览（PageSift {meta['pagesift']}）\n",
          f"PageSift {meta['pagesift']} 新测；竞品为 2026-10-21/22/27/28 的原始数据，未重跑。"
          "每行一个工作负载，PageSift 取该负载的最佳配置；内存单列，不单独进榜。\n",
          "\n### 大幅领先\n", table(far, "zh"), "\n### 领先\n", table(ahead, "zh"), "\n"]
    g = gap_sentence(gaps)
    if g:
        zh.append(("最大差距：" if gap_kind == "behind" else "最接近的对照：") + g + "\n")
        zh.append("\n" + A2_RATE["zh"] + "\n")
    (out / "headline-zh.md").write_text("\n".join(zh), encoding="utf-8")

    en = [f"## Performance at a glance (PageSift {meta['pagesift']})\n",
          f"PageSift {meta['pagesift']} was re-measured; competitor numbers are the recorded 2026-10-21/22/27/28 data "
          "and were not re-run. One row per workload, best PageSift configuration; memory is a column, not a separate rank.\n",
          "\n### Far ahead\n", table(far, "en"), "\n### Ahead\n", table(ahead, "en"), "\n"]
    g_en = gap_sentence(gaps, "en")
    if g_en:
        en.append(("Largest gaps: " if gap_kind == "behind" else "Closest comparisons: ") + g_en + "\n")
        en.append("\n" + A2_RATE["en"] + "\n")
    (out / "headline-en.md").write_text("\n".join(en), encoding="utf-8")

    data = {
        "meta": meta,
        "rows": [{k: v for k, v in r.items() if not k.startswith("_")} | {
            "psift_value": r["_psift"], "tier": r["_tier"], "ratio": r["_ratio"], "delta": r["_delta"],
            "psift_mem_value": r["_pm"], "rival_mem_value": r["_rm"],
        } for r in rows],
        "gap_kind": gap_kind,
        "missing": missing,
    }
    (out / "scorecard.json").write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")
    (out / "metrics.json").write_text(json.dumps(metrics, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"scorecard: {out/'report.md'}")
    print(f"headline:  {out/'headline-zh.md'}, {out/'headline-en.md'}")
    print(f"far={len(far)} ahead={len(ahead)} gaps={len(gaps)} missing={len(missing)}")
    if a.json:
        print(json.dumps(data, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()

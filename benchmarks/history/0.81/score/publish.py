#!/usr/bin/env python3
"""Publish the 0.81 two-tier view into the four READMEs and docs/benchmarks.md.

    python3 -I publish.py <scorecard.json> [--stamp <run-stamp>] [--write]

Without --write it prints the blocks and changes nothing.  With --write it
replaces the existing <!-- perf-glance --> blocks in README.md,
docs/README.zh-CN.md, docs/README.ja.md and docs/README.es.md, and inserts a
"0.81 per-domain refresh" section at the top of docs/benchmarks.md.

The homepage keeps only Far ahead / Ahead.  The old "Behind but close" and
"Behind" tables in README.md are left alone by default; pass --homepage to
replace the whole "How PageSift compares" section with one gap sentence and a
link to the full report.
"""
import argparse
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
FILES = {
    "en": REPO / "README.md",
    "zh": REPO / "docs/README.zh-CN.md",
    "ja": REPO / "docs/README.ja.md",
    "es": REPO / "docs/README.es.md",
}
LABEL = {
    "en": dict(title="Performance at a glance", far="Far ahead", ahead="Ahead", gaps="Largest gaps",
               home="How PageSift compares", behind="Still behind", closest="Closest comparisons",
               head=["Domain", "Workload", "Metric", "PageSift 0.81", "Fastest rival", "Rival", "Gap"],
               mem="Peak MB/worker (PageSift / rival)",
               a2note="A2 was re-fetched on 2026-10-29; its absolute times are not directly comparable with the 10-28 A2.",
               intro="PageSift {v} was re-measured on {date}; competitor numbers are the recorded 2026-10-21/22/27/28 data and were not re-run. One row per workload, best PageSift configuration; memory is a column, not a separate rank.",
               report="Full report"),
    "zh": dict(title="性能一览", far="大幅领先", ahead="领先", gaps="最大差距",
               home="和同类工具比", behind="仍落后", closest="最接近的对照",
               head=["领域", "工作负载", "指标", "PageSift 0.81", "最快对手", "对手", "差距"],
               mem="峰值内存 MB（PageSift / 对手）",
               a2note="A2 的 269 页于 2026-10-29 重新抓取，绝对耗时不能和 10-28 的 A2 直接比较。",
               intro="PageSift {v} 于 {date} 新测；竞品为 2026-10-21/22/27/28 的原始数据，未重跑。每行一个工作负载，PageSift 取该负载的最佳配置；内存单列，不单独进榜。",
               report="完整报告"),
    "ja": dict(title="パフォーマンス概要", far="大幅リード", ahead="リード", gaps="最大の差",
               home="他ツールとの比較", behind="まだ後れている項目", closest="最も接近した比較",
               head=["分野", "ワークロード", "指標", "PageSift 0.81", "最速の競合", "競合", "差"],
               mem="ピークメモリ MB（PageSift / 競合）",
               a2note="A2 は 2026-10-29 に再取得したため、絶対時間は 10-28 の A2 と直接比較できません。",
               intro="PageSift {v} を {date} に再計測。競合の数値は 2026-10-21/22/27/28 の記録で、再実行していません。1 行 = 1 ワークロード、PageSift は最良の構成。メモリは列で、別の順位にはしません。",
               report="完全なレポート"),
    "es": dict(title="Rendimiento de un vistazo", far="Muy por delante", ahead="Por delante", gaps="Mayores diferencias",
               home="Cómo se compara PageSift", behind="Aún por detrás", closest="Comparaciones más cercanas",
               head=["Dominio", "Carga de trabajo", "Métrica", "PageSift 0.81", "Rival más rápido", "Rival", "Diferencia"],
               mem="MB pico/trabajador (PageSift / rival)",
               a2note="A2 se volvió a capturar el 2026-10-29; sus tiempos absolutos no son comparables con los del A2 del 10-28.",
               intro="PageSift {v} se midió de nuevo el {date}; los números de la competencia son los registros del 2026-10-21/22/27/28 y no se repitieron. Una fila por carga de trabajo, la mejor configuración de PageSift; la memoria es una columna, no una clasificación aparte.",
               report="Informe completo"),
}


def unit(row):
    m = row["metric_en"]
    for u in ("pages/s", "files/s", "ms/page", "ms/question", "total ms", "seconds", "peak memory"):
        if u in m:
            return u
    return ""


def fmt(row, v):
    u = unit(row)
    if u in ("pages/s", "files/s", "peak memory"):
        return f"{v:,.1f}"
    if u == "ms/page":
        return f"{v:.1f}" if v >= 1 else f"{v:.2f}"
    if u == "ms/question":
        return f"{v:,.1f}"
    if u == "total ms":
        return f"{v:,.0f}"
    if u == "seconds":
        return f"{v:.1f}"
    return f"{v:.3f}"


def gap(row):
    if row.get("kind") == "speed":
        return f"×{row['ratio']:.2f}"
    return f"{row['delta']:+.3f}"


def mem_cell(r):
    pm, rm = r.get("psift_mem_value"), r.get("rival_mem_value")
    if pm is None and rm is None:
        return "—"
    return f"{pm:,.0f} / {rm:,.0f}" if pm is not None and rm is not None else f"{pm:,.0f} / —" if pm is not None else f"— / {rm:,.0f}"


def table(rows, lang):
    L = LABEL[lang]
    show_mem = any(r.get("psift_mem_value") is not None or r.get("rival_mem_value") is not None for r in rows)
    head = list(L["head"])
    if show_mem:
        head.append(L["mem"])
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    for r in rows:
        domain = r["domain_zh"] if lang == "zh" else r["domain_en"]
        workload = r["workload_zh"] if lang == "zh" else r["workload_en"]
        metric = r["metric_zh"] if lang == "zh" else r["metric_en"]
        cells = [domain, workload, metric, f"**{fmt(r, r['psift_value'])}**", r["rival_name"],
                 fmt(r, r["rival"]), f"**{gap(r)}**"]
        if show_mem:
            cells.append(mem_cell(r))
        out.append("| " + " | ".join(cells) + " |")
    return "\n".join(out)


def gap_line(rows, lang):
    parts = []
    for r in rows:
        w = r["workload_zh"] if lang == "zh" else r["workload_en"]
        u = unit(r)
        suffix = f" {u}" if r.get("kind") == "speed" and u else ""
        parts.append(f"{w} {fmt(r, r['psift_value'])}{suffix} vs {r['rival_name']} {fmt(r, r['rival'])}{suffix}")
    sep = "；" if lang == "zh" else "; "
    end = "。" if lang == "zh" else "."
    return sep.join(parts) + end


def gap_rows(data):
    """Same choice as summarize.py: real behind rows, else the closest few."""
    def sev(r):
        return r["ratio"] if r.get("kind") == "speed" and r.get("ratio") is not None else 1.0 + (r.get("delta") or 0.0)

    rows = [r for r in data["rows"] if r.get("psift_value") is not None and not r.get("appendix")]
    behind = sorted([r for r in rows if r["tier"] == "behind"], key=sev)[:3]
    if behind:
        return behind, "behind"
    return sorted([r for r in rows if r["tier"] in ("parity", "ahead", "far")], key=sev)[:3], "closest"


def row_strength(r):
    if r.get("ratio") is not None:
        return r["ratio"]
    return 1.0 + (r.get("delta") or 0.0)


def perf_block(data, lang, link, show_gaps=True):
    L = LABEL[lang]
    far = sorted([r for r in data["rows"] if r["tier"] == "far" and r.get("headline", True) and not r.get("appendix")],
                 key=row_strength, reverse=True)
    ahead = sorted([r for r in data["rows"] if r["tier"] == "ahead" and r.get("headline", True) and not r.get("appendix")],
                   key=row_strength, reverse=True)
    gaps, gap_kind = gap_rows(data)
    s = [f"## {L['title']} ({data['meta']['pagesift']})\n",
         L["intro"].format(v=data["meta"]["pagesift"], date=data["meta"]["date"]),
         f"\n\n### {L['far']}\n", table(far, lang), f"\n\n### {L['ahead']}\n", table(ahead, lang)]
    if gaps and show_gaps:
        label = L["gaps"] if gap_kind == "behind" else L["closest"]
        sep = "：" if lang in ("zh", "ja") else ":"
        s += [f"\n\n**{label}{sep}** {gap_line(gaps, lang)}"]
    s += [f"\n\n*{L['a2note']}*", f"\n\n[{L['report']}]({link})\n"]
    return "\n".join(s)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scorecard", type=Path)
    ap.add_argument("--stamp")
    ap.add_argument("--write", action="store_true")
    ap.add_argument("--homepage", action="store_true", help="also replace the old Behind tables in README.md")
    a = ap.parse_args()
    data = json.loads(a.scorecard.read_text(encoding="utf-8"))
    stamp = a.stamp or a.scorecard.parent.name
    for lang, path in FILES.items():
        link = f"benchmarks/history/0.81/results/{stamp}/report.md" if lang == "en" else f"../benchmarks/history/0.81/results/{stamp}/report.md"
        block = f"<!-- perf-glance -->\n{perf_block(data, lang, link, show_gaps=not a.homepage)}\n<!-- /perf-glance -->\n\n---\n\n"
        s = path.read_text(encoding="utf-8")
        if "<!-- perf-glance -->" in s:
            s = re.sub(r"<!-- perf-glance -->.*?<!-- /perf-glance -->\n\n---\n\n", block, s, flags=re.S)
        else:
            i = s.index("</div>")
            j = s.index("\n---\n", i) + len("\n---\n")
            s = s[:j] + "\n" + block + s[j:].lstrip("\n")
        if a.homepage:
            gaps, gap_kind = gap_rows(data)
            line = gap_line(gaps, lang)
            label = LABEL[lang]["behind"] if gap_kind == "behind" else LABEL[lang]["closest"]
            sep = "：" if lang in ("zh", "ja") else ":"
            repl = (f"## {LABEL[lang]['home']}\n\n"
                    f"**{label}{sep}** {line}\n\n"
                    f"[{LABEL[lang]['report']}]({link}).\n\n---\n\n")
            s, n = re.subn(r"## (?:How PageSift compares|和同类工具比|他ツールとの比較|Cómo se compara PageSift).*?\n---\n\n",
                           repl, s, count=1, flags=re.S)
            if n != 1:
                print(f"warning: homepage section not found in {path.relative_to(REPO)}", file=sys.stderr)
        if a.write:
            path.write_text(s, encoding="utf-8")
            print(f"updated {path.relative_to(REPO)}")
        else:
            print(f"===== {path.relative_to(REPO)} =====\n{block}")

    # docs/benchmarks.md: insert/replace a top-level 0.81 section.
    p = REPO / "docs/benchmarks.md"
    s = p.read_text(encoding="utf-8")
    block = ("<!-- 081-refresh -->\n## 0.81 per-domain refresh (2026-10-29)\n\n"
             + perf_block(data, "en", f"../benchmarks/history/0.81/results/{stamp}/report.md").replace("## Performance at a glance (0.81)", "### Performance at a glance")
             + "\n<!-- /081-refresh -->\n\n")
    if "<!-- 081-refresh -->" in s:
        s = re.sub(r"<!-- 081-refresh -->.*?<!-- /081-refresh -->\n\n", block, s, flags=re.S)
    elif "## 0.80.9 head-to-head" in s:
        s = s.replace("## 0.80.9 head-to-head", block + "## 0.80.9 head-to-head", 1)
    else:
        s = block + s
    if a.write:
        p.write_text(s, encoding="utf-8")
        print(f"updated {p.relative_to(REPO)}")
    else:
        print(f"===== {p.relative_to(REPO)} =====\n{block}")


if __name__ == "__main__":
    main()

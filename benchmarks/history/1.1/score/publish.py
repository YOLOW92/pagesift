#!/usr/bin/env python3
"""Publish the 1.1 two-tier view into the four READMEs and docs/benchmarks.md.

    python3 -I publish.py <scorecard.json> [--stamp <run-stamp>] [--write]

Without --write it prints the blocks and changes nothing.  With --write it
replaces the existing <!-- perf-glance --> blocks in README.md,
docs/README.zh-CN.md, docs/README.ja.md and docs/README.es.md, and inserts a
"1.1 per-domain refresh" section at the top of docs/benchmarks.md.

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
HIST = Path(__file__).resolve().parents[1]
FILES = {
    "en": REPO / "README.md",
    "zh": REPO / "docs/README.zh-CN.md",
    "ja": REPO / "docs/README.ja.md",
    "es": REPO / "docs/README.es.md",
}
LABEL = {
    "en": dict(title="Performance at a glance", far="Far ahead", ahead="Ahead", gaps="Largest gaps",
               home="How PageSift compares", behind="Still behind", closest="Closest comparisons",
               head=["Domain", "Workload", "Metric", "PageSift 1.1", "Fastest rival", "Rival", "Gap"],
               mem="Peak MB/worker (PageSift / rival)",
               a2rate="A2 throughput is measured against a replay server on a single loopback address, so all 269 pages count as one host. PageSift waits 1 s between requests to the same host by default; spider has no such limit. This row mostly reflects that politeness interval, not engine speed.",
               a2note="A2 was re-fetched on 2026-10-29; its absolute times are not directly comparable with the 10-28 A2.",
               intro="PageSift {v} was re-measured on {date}; competitor numbers are the recorded 2026-10-21/22/27/28 data and were not re-run. One row per workload, best PageSift configuration; memory is a column, not a separate rank.",
               report="Full report"),
    "zh": dict(title="性能一览", far="大幅领先", ahead="领先", gaps="最大差距",
               home="和同类工具比", behind="仍落后", closest="最接近的对照",
               head=["领域", "工作负载", "指标", "PageSift 1.1", "最快对手", "对手", "差距"],
               mem="峰值内存 MB（PageSift / 对手）",
               a2rate="A2 吞吐是对一个回环地址上的回放服务测的，269 页都算同一个主机。PageSift 默认对同一主机两次请求之间间隔 1 秒，spider 不限速；这一行主要反映这个礼貌间隔，而不是引擎速度。",
               a2note="A2 的 269 页于 2026-10-29 重新抓取，绝对耗时不能和 10-28 的 A2 直接比较。",
               intro="PageSift {v} 于 {date} 新测；竞品为 2026-10-21/22/27/28 的原始数据，未重跑。每行一个工作负载，PageSift 取该负载的最佳配置；内存单列，不单独进榜。",
               report="完整报告"),
    "ja": dict(title="パフォーマンス概要", far="大幅リード", ahead="リード", gaps="最大の差",
               home="他ツールとの比較", behind="まだ後れている項目", closest="最も接近した比較",
               head=["分野", "ワークロード", "指標", "PageSift 1.1", "最速の競合", "競合", "差"],
               mem="ピークメモリ MB（PageSift / 競合）",
               a2rate="A2 のスループットは単一のループバックアドレス上のリプレイサーバーに対して測定しており、269 ページすべてが同じホストになります。PageSift は既定で同一ホストへのリクエスト間隔を 1 秒空けますが、spider にはこの制限がありません。この行の差は主にこの間隔によるもので、エンジン速度の差ではありません。",
               a2note="A2 は 2026-10-29 に再取得したため、絶対時間は 10-28 の A2 と直接比較できません。",
               intro="PageSift {v} を {date} に再計測。競合の数値は 2026-10-21/22/27/28 の記録で、再実行していません。1 行 = 1 ワークロード、PageSift は最良の構成。メモリは列で、別の順位にはしません。",
               report="完全なレポート"),
    "es": dict(title="Rendimiento de un vistazo", far="Muy por delante", ahead="Por delante", gaps="Mayores diferencias",
               home="Cómo se compara PageSift", behind="Aún por detrás", closest="Comparaciones más cercanas",
               head=["Dominio", "Carga de trabajo", "Métrica", "PageSift 1.1", "Rival más rápido", "Rival", "Diferencia"],
               mem="MB pico/trabajador (PageSift / rival)",
               a2rate="El rendimiento de A2 se mide contra un servidor de reproducción en una sola dirección loopback, así que las 269 páginas cuentan como un mismo host. PageSift espera por defecto 1 s entre peticiones al mismo host; spider no tiene ese límite. Esta fila refleja sobre todo ese intervalo de cortesía, no la velocidad del motor.",
               a2note="A2 se volvió a capturar el 2026-10-29; sus tiempos absolutos no son comparables con los del A2 del 10-28.",
               intro="PageSift {v} se midió de nuevo el {date}; los números de la competencia son los registros del 2026-10-21/22/27/28 y no se repitieron. Una fila por carga de trabajo, la mejor configuración de PageSift; la memoria es una columna, no una clasificación aparte.",
               report="Informe completo"),
}

COMPACT = {'en': {'chead': ['Task', 'Metric', 'PageSift', 'Best other tool', 'Gap'], 'legend': '↑ higher is better · ↓ lower is better. Strongest result per domain; methods and sources in the full report.', 'up': '↑', 'down': '↓'},
    'zh': {'chead': ['任务', '指标', 'PageSift', '最好的其他工具', '差距'], 'legend': '↑ 越高越好 · ↓ 越低越好。每个领域取最强的一项；方法和出处见完整报告。', 'up': '↑', 'down': '↓'},
    'ja': {'chead': ['タスク', '指標', 'PageSift', '最良の他ツール', '差'], 'legend': '↑ 高いほど良い · ↓ 低いほど良い。分野ごとに最も強い 1 項目。方法と出典は完全なレポートに。', 'up': '↑', 'down': '↓'},
    'es': {'chead': ['Tarea', 'Métrica', 'PageSift', 'Mejor otra herramienta', 'Diferencia'], 'legend': '↑ más alto es mejor · ↓ más bajo es mejor. El resultado más fuerte por dominio; métodos y fuentes en el informe completo.', 'up': '↑', 'down': '↓'}}


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


def compact_rows(data):
    """首页表：全部领先的行，按领域聚在一起，领域按它最强的一行排序。"""
    rows = sorted([r for r in data["rows"] if r["tier"] in ("far", "ahead") and r.get("headline", True) and not r.get("appendix")],
                  key=row_strength, reverse=True)
    order = {}
    for r in rows:
        order.setdefault(r["domain_en"], len(order))
    return sorted(rows, key=lambda r: order[r["domain_en"]])


MEM_HEAD = {
    "en": ["Peak memory per worker", "PageSift MB ↓", "Other tool MB", "Less memory"],
    "zh": ["每 worker 峰值内存", "PageSift MB ↓", "其他工具 MB", "内存少"],
    "ja": ["ワーカーあたりのピークメモリ", "PageSift MB ↓", "他ツール MB", "メモリ削減"],
    "es": ["Memoria pico por worker", "PageSift MB ↓", "Otra herramienta MB", "Menos memoria"],
}


XBERG_TEXT = {
    "en": dict(head=["Metric", "PageSift CLI", "PageSift server", "xberg", "xberg layout mode"],
               title="**PageSift 1.1 and xberg 1.3.9.** xberg measured on {date}; a range is two runs. A bold value is the best in its row.",
               rows=["PDF B1, olmOCR score 1,269 pages ↑", "Office B4, structure kept, hold-out 39 files ↑",
                     "PDF B1, 1 worker, files/s ↑", "PDF B1, 16 workers, files/s ↑",
                     "Office B4, 1 worker, files/s ↑", "Office B4, 16 workers, files/s ↑",
                     "PDF B1, 1 worker, peak MB ↓", "Office B4, 1 worker, peak MB ↓",
                     "Office B4, 1 worker, median ms per file ↓"]),
    "zh": dict(head=["指标", "PageSift CLI", "PageSift 服务", "xberg", "xberg 版面模式"],
               title="**PageSift 1.1 和 xberg 1.3.9。** xberg 测于 {date}；范围是两轮的结果。加粗的是该行最好的值。",
               rows=["PDF B1，olmOCR 总分 1,269 页 ↑", "Office B4，结构保留率，留出集 39 份 ↑",
                     "PDF B1，1 路，份/秒 ↑", "PDF B1，16 路，份/秒 ↑",
                     "Office B4，1 路，份/秒 ↑", "Office B4，16 路，份/秒 ↑",
                     "PDF B1，1 路，峰值 MB ↓", "Office B4，1 路，峰值 MB ↓",
                     "Office B4，1 路，单份中位毫秒 ↓"]),
    "ja": dict(head=["指標", "PageSift CLI", "PageSift サーバー", "xberg", "xberg レイアウトモード"],
               title="**PageSift 1.1 と xberg 1.3.9。** xberg は {date} に測定。範囲は 2 回の結果。太字は行内で最良の値。",
               rows=["PDF B1、olmOCR スコア 1,269 ページ ↑", "Office B4、構造保持率、ホールドアウト 39 件 ↑",
                     "PDF B1、1 ワーカー、件/秒 ↑", "PDF B1、16 ワーカー、件/秒 ↑",
                     "Office B4、1 ワーカー、件/秒 ↑", "Office B4、16 ワーカー、件/秒 ↑",
                     "PDF B1、1 ワーカー、ピーク MB ↓", "Office B4、1 ワーカー、ピーク MB ↓",
                     "Office B4、1 ワーカー、1 件の中央値 ms ↓"]),
    "es": dict(head=["Métrica", "PageSift CLI", "PageSift servidor", "xberg", "xberg modo de maquetación"],
               title="**PageSift 1.1 y xberg 1.3.9.** xberg medido el {date}; un rango son dos ejecuciones. En negrita, el mejor valor de la fila.",
               rows=["PDF B1, puntuación olmOCR 1.269 páginas ↑", "Office B4, estructura conservada, reserva 39 archivos ↑",
                     "PDF B1, 1 worker, archivos/s ↑", "PDF B1, 16 workers, archivos/s ↑",
                     "Office B4, 1 worker, archivos/s ↑", "Office B4, 16 workers, archivos/s ↑",
                     "PDF B1, 1 worker, MB pico ↓", "Office B4, 1 worker, MB pico ↓",
                     "Office B4, 1 worker, mediana ms por archivo ↓"]),
}


def xberg_block(lang, metrics):
    """PageSift 这一轮的数字和已记录的 xberg 数字放一张表，输的也照写。"""
    x = json.loads((HIST / "results/xberg-1.3.9/summary.json").read_text(encoding="utf-8"))
    d, lay = x["default"], x["layout"]
    T = XBERG_TEXT[lang]

    def rng(key, field, digits):
        v = [r[field] for r in d[key]]
        lo, hi = min(v), max(v)
        a, b = f"{lo:.{digits}f}", f"{hi:.{digits}f}"
        return (a if a == b else f"{a}–{b}"), (hi if field == "tput" else lo)

    def layout(key, field, digits):
        r = lay.get(key)
        return (f"{r[0][field]:.{digits}f}", r[0][field]) if r else ("—", None)

    def m(key, digits):
        v = metrics.get(key)
        return (f"{v:.{digits}f}", v) if v is not None else ("—", None)

    spec = [
        (m("quality.b1.olmocr", 3), ("—", None), (f"{d['olmocr']:.3f}", d["olmocr"]), (f"{lay['olmocr']:.3f}", lay["olmocr"]), True),
        (m("quality.b4.holdout", 3), ("—", None), (f"{d['b4.holdout']:.3f}", d["b4.holdout"]), ("—", None), True),
        (m("v2.b1.cli.1w.tput", 1), m("v2.b1.serve.1w.tput", 1), rng("b1.w1", "tput", 1), layout("b1.w1", "tput", 2), True),
        (m("v2.b1.cli.16w.tput", 1), m("v2.b1.serve.16w.tput", 1), rng("b1.w16", "tput", 1), layout("b1.w16", "tput", 2), True),
        (m("v2.b4.cli.1w.tput", 1), m("v2.b4.serve.1w.tput", 1), rng("b4.w1", "tput", 1), ("—", None), True),
        (m("v2.b4.cli.16w.tput", 1), m("v2.b4.serve.16w.tput", 1), rng("b4.w16", "tput", 1), ("—", None), True),
        (m("v2.b1.cli.1w.peak", 0), m("v2.b1.serve.1w.peak", 0), rng("b1.w1", "peak", 0), layout("b1.w1", "peak", 0), False),
        (m("v2.b4.cli.1w.peak", 0), m("v2.b4.serve.1w.peak", 0), rng("b4.w1", "peak", 0), ("—", None), False),
        (m("v2.b4.cli.1w.p50", 1), m("v2.b4.serve.1w.p50", 1), rng("b4.w1", "p50", 1), ("—", None), False),
    ]
    out = ["", T["title"].format(date=x["date"]), "", "| " + " | ".join(T["head"]) + " |", "|---|---:|---:|---:|---:|"]
    for label, (cli, srv, xb, la, up) in zip(T["rows"], spec):
        vals = [c[1] for c in (cli, srv, xb, la) if c[1] is not None]
        best = max(vals) if up else min(vals)
        cells = [f"**{c[0]}**" if c[1] == best else c[0] for c in (cli, srv, xb, la)]
        out.append(f"| {label} | " + " | ".join(cells) + " |")
    return "\n".join(out) + "\n"


def read_metrics(path):
    out = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        k, _, v = line.partition("\t")
        try:
            out[k] = float(v)
        except ValueError:
            pass
    return out


def compact_block(data, lang, link):
    """按模板改版后的首页性能段：一张精简表 + 落后项 + 口径说明 + 完整报告链接。"""
    L, C = LABEL[lang], COMPACT[lang]
    out = ["| " + " | ".join(C["chead"]) + " |", "|---|---|---:|---|---:|"]
    for r in compact_rows(data):
        workload = r["workload_zh"] if lang == "zh" else r["workload_en"]
        domain = r["domain_zh"] if lang == "zh" else r["domain_en"]
        metric = r["metric_zh"] if lang == "zh" else r["metric_en"]
        arrow = C["up"] if r.get("direction") == "up" else C["down"]
        out.append(f"| {domain} — {workload} | {metric} {arrow} | **{fmt(r, r['psift_value'])}** | "
                   f"{r['rival_name']} {fmt(r, r['rival'])} | **{gap(r)}** |")
    gaps, gap_kind = gap_rows(data)
    label = L["behind"] if gap_kind == "behind" else L["closest"]
    base = "docs/assets" if lang == "en" else "assets"
    charts = []
    for name, alt in (("scaling", "Throughput from 1 to 16 workers, PageSift against other tools"),
                      ("versions", "Scanned-PDF olmOCR score by version against Docling"),
                      ("memory", "Times less memory than other tools"), ("quality", "Quality against the next best tool")):
        if (REPO / "docs/assets" / f"perf-{name}-light.svg").exists():
            charts.append(f'<picture><source media="(prefers-color-scheme: dark)" srcset="{base}/perf-{name}-dark.svg">'
                          f'<img src="{base}/perf-{name}-light.svg" alt="{alt}" width="860"></picture>\n')
    mem_rows = []
    if data.get("_memory"):
        mem_rows = ["\n| " + " | ".join(MEM_HEAD[lang]) + " |", "|---|---:|---|---:|"]
        for m in data["_memory"]:
            mem_rows.append(f"| {m['label_zh'] if lang == 'zh' else m['label_en']} | **{m['psift_value']:.0f}** | "
                            f"{m['rival_name']} {m['rival']:,} ({m['rival_date']}) | **×{m['ratio']:.1f}** |")
    xberg = [xberg_block(lang, data["_metrics"])] if data.get("_metrics") else []
    return "\n".join([*charts, C["legend"] + "\n", *out, *mem_rows, *xberg,
                      f"\n<details>\n<summary>{label}</summary>\n\n{gap_line(gaps, lang)}\n\n</details>",
                      f"\n[{L['report']}]({link})\n"])


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
        s += [f"\n\n**{label}{sep}** {gap_line(gaps, lang)}", f"\n\n*{L['a2rate']}*"]
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
    mem = a.scorecard.with_name("memory.json")
    if mem.exists():
        data["_memory"] = json.loads(mem.read_text(encoding="utf-8"))
    metrics_file = a.scorecard.with_name("metrics.tsv")
    if metrics_file.exists():
        data["_metrics"] = read_metrics(metrics_file)
    stamp = a.stamp or a.scorecard.parent.name
    for lang, path in FILES.items():
        link = f"benchmarks/history/1.1/results/{stamp}/report.md" if lang == "en" else f"../benchmarks/history/1.1/results/{stamp}/report.md"
        s = path.read_text(encoding="utf-8")
        if "<!-- readme-template: v2 -->" in s:
            # 按模板改版后：性能段里只放精简表，不再有首页大表和 How PageSift compares
            block = f"<!-- perf-glance -->\n{compact_block(data, lang, link)}\n<!-- /perf-glance -->"
            s = re.sub(r"<!-- perf-glance -->.*?<!-- /perf-glance -->", lambda _: block, s, flags=re.S)
            if a.write:
                path.write_text(s, encoding="utf-8")
                print(f"updated {path.relative_to(REPO)}")
            else:
                print(f"===== {path.relative_to(REPO)} =====\n{block}")
            continue
        block = f"<!-- perf-glance -->\n{perf_block(data, lang, link, show_gaps=not a.homepage)}\n<!-- /perf-glance -->\n\n---\n\n"
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
                    f"*{LABEL[lang]['a2rate']}*\n\n"
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

    # docs/benchmarks.md: insert/replace a top-level 1.1 section.
    p = REPO / "docs/benchmarks.md"
    s = p.read_text(encoding="utf-8")
    block = ("<!-- 081-refresh -->\n## 1.1 per-domain refresh (" + data["meta"]["date"] + ")\n\n"
             + perf_block(data, "en", f"../benchmarks/history/1.1/results/{stamp}/report.md").replace("## Performance at a glance (1.0)", "### Performance at a glance")
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

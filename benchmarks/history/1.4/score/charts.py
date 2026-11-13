#!/usr/bin/env python3
"""Draw the README charts from a scorecard.json: static SVG, one light and one dark file per chart.

    python3 -I charts.py <scorecard.json> <metrics.tsv> <output dir>

scaling  throughput at 1 and 16 workers, a line per tool, from [[scale]] in scorecard.toml
versions scanned-PDF score by version against Docling, from [versions] in scorecard.toml
quality  PageSift and the next best tool on the same 0-1 score
memory   how many times less memory than a named tool, from [[memory]] in scorecard.toml
"""
import json
import math
import sys
import tomllib
from html import escape
from pathlib import Path

THEMES = {
    "light": dict(ink="#0b0b0b", muted="#52514e", grid="#e4e3df", ours="#2a78d6", other="#eb6834"),
    "dark": dict(ink="#f0efec", muted="#c3c2b7", grid="#3a3a37", ours="#3987e5", other="#d95926"),
}
WIDTH = 860
LEFT = 300  # 左边放标签的宽度
RIGHT = 250  # 右边放数值的宽度
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"
BAR = 16
RADIUS = 4


def shown(rows):
    return [r for r in rows if r["tier"] in ("far", "ahead") and r.get("headline", True) and not r.get("appendix")]


def num(v):
    if v >= 100:
        return f"{v:,.0f}"
    if v >= 10:
        return f"{v:.1f}"
    if v >= 1:
        return f"{v:.2f}".rstrip("0").rstrip(".")
    return f"{v:.3f}".rstrip("0").rstrip(".")


def bar(x, y, w, color, h=BAR):
    """基线一侧是直角，数据一端圆角。"""
    w = max(w, 2)
    r = min(RADIUS, w)
    return (f'<path d="M{x} {y}h{w - r:.1f}a{r} {r} 0 0 1 {r} {r}v{h - 2 * r}a{r} {r} 0 0 1 -{r} {r}h-{w - r:.1f}z" '
            f'fill="{color}"/>')


def text(x, y, s, fill, size=13, weight=400, anchor="start"):
    return (f'<text x="{x}" y="{y}" fill="{fill}" font-size="{size}" font-weight="{weight}" '
            f'text-anchor="{anchor}">{escape(s)}</text>')


def frame(title, note, body, height, t):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {height}" width="{WIDTH}" height="{height}" '
            f'role="img" aria-label="{escape(title)}" font-family="{FONT}">\n<title>{escape(title)}</title>\n'
            + text(0, 22, title, t["ink"], 17, 600) + "\n" + text(0, 44, note, t["muted"], 13) + "\n"
            + "\n".join(body) + "\n</svg>\n")


def legend(y, t, other="Next best tool"):
    return [bar(0, y, 14, t["ours"], 10), text(20, y + 10, "PageSift", t["muted"], 12),
            bar(90, y, 14, t["other"], 10), text(110, y + 10, other, t["muted"], 12)]


def paired(rows, t, title, note, value, other_value, scale):
    span = WIDTH - LEFT - RIGHT
    body, y = legend(56, t), 90
    for r in rows:
        ours, rival = value(r), other_value(r)
        body.append(text(0, y + 11, r["domain_en"], t["ink"], 13, 600))
        body.append(text(0, y + 28, r["workload_en"], t["muted"], 12))
        for k, (v, color, weight, name) in enumerate(((ours, t["ours"], 600, ""), (rival, t["other"], 400, r["rival_name"]))):
            w = span * v / scale
            yy = y + k * (BAR + 2)
            body.append(bar(LEFT, yy, w, color))
            body.append(text(LEFT + w + 8, yy + 13, f"{v:.3f} {name}".strip(), t["ink"] if k == 0 else t["muted"], 13, weight))
        y += 54
    return frame(title, note, body, y - 6, t)


def quality(rows, t):
    rows = [r for r in rows if r["kind"] == "quality" and r["direction"] == "up" and r.get("rival") and r["psift_value"] <= 1]
    rows.sort(key=lambda r: r["rival"] - r["psift_value"])
    if not rows:
        return None
    return paired(rows, t, "Quality against the next best tool", "Scores from 0 to 1. Higher is better.",
                  lambda r: r["psift_value"], lambda r: r["rival"], 1.0)


def memory_rows(metrics, card):
    out = []
    for m in card.get("memory", []):
        v = metrics.get(m["psift"])
        if v and v < m["rival"]:
            out.append({**m, "psift_value": v, "ratio": m["rival"] / v})
    return sorted(out, key=lambda r: -r["ratio"])


def memory(rows, t):
    if not rows:
        return None
    span = WIDTH - LEFT - RIGHT
    top = math.log10(max(r["ratio"] for r in rows))
    body, y = [], 70
    for r in rows:
        w = span * math.log10(r["ratio"]) / top
        body.append(text(0, y + 7, r["label_en"], t["ink"]))
        body.append(text(0, y + 24, f"{num(r['psift_value'])} MB vs {r['rival_name']} {num(r['rival'])} MB", t["muted"], 12))
        body.append(bar(LEFT, y - 4, w, t["ours"]))
        body.append(text(LEFT + w + 8, y + 9, f"×{r['ratio']:.1f}", t["ink"], 14, 600))
        y += 46
    return frame("Times less memory than other tools", "Peak MB per worker on the same workload. Higher is better. Bars use a log scale.",
                 body, y - 8, t)


PALETTE = {"light": ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4"], "dark": ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181"]}


def spread(ys, gap=15):
    """把同一列的标签上下推开，不重叠。"""
    order = sorted(range(len(ys)), key=lambda i: ys[i])
    out = list(ys)
    for a, b in zip(order, order[1:]):
        if out[b] - out[a] < gap:
            out[b] = out[a] + gap
    return out


def scale_panel(x0, width, top, height, title, series, lo, hi, t, colors):
    y_of = lambda v: top + height - height * (math.log10(v) - math.log10(lo)) / (math.log10(hi) - math.log10(lo))
    left, right = x0 + 44, x0 + width - 150
    body = [text(x0, top - 16, title, t["ink"], 14, 600)]
    tick = 10 ** math.floor(math.log10(lo))
    while tick <= hi:
        if tick >= lo:
            y = y_of(tick)
            body.append(f'<line x1="{left}" y1="{y:.1f}" x2="{right}" y2="{y:.1f}" stroke="{t["grid"]}"/>')
            body.append(text(left - 8, y + 4, f"{tick:g}", t["muted"], 11, 400, "end"))
        tick *= 10
    for x, label in ((left, "1 worker"), (right, "16 workers")):
        body.append(text(x, top + height + 20, label, t["muted"], 12, 400, "middle"))
    labels = spread([y_of(s["w16"]) + 4 for s in series])
    for i, s in reversed(list(enumerate(series))):
        c = colors[i % len(colors)]
        y1, y2 = y_of(s["w1"]), y_of(s["w16"])
        body.append(f'<line x1="{left}" y1="{y1:.1f}" x2="{right}" y2="{y2:.1f}" stroke="{c}" stroke-width="{3 if i == 0 else 2}" stroke-linecap="round"/>')
        for x, y in ((left, y1), (right, y2)):
            body.append(f'<circle cx="{x}" cy="{y:.1f}" r="4" fill="{c}"/>')
        body.append(text(right + 10, labels[i], f"{s['tool']} {num(s['w16'])}", t["ink"] if i == 0 else t["muted"], 12, 600 if i == 0 else 400))
        body.append(text(left + 8, y1 - 7, num(s["w1"]), t["muted"], 11) if i == 0 else "")
    return body


def scaling(panels, t, colors):
    if not panels:
        return None
    width = (WIDTH - 20) // 2
    top, height = 96, 300
    body = []
    for k, (title, series) in enumerate(panels.items()):
        vals = [v for s in series for v in (s["w1"], s["w16"])]
        body += scale_panel(k * (width + 20), width, top, height, title, series, min(vals) * 0.7, max(vals) * 1.4, t, colors)
    return frame("Throughput as workers increase", "Items per second, log scale, PageSift run as a CLI. Higher is better.", [b for b in body if b], top + height + 34, t)


def version_chart(rows, t, colors):
    if not rows:
        return None
    left, right, top, height = 50, WIDTH - 250, 80, 260
    ys = lambda v: top + height - height * v / 0.7
    labels = ["0.81", "1.0", "1.4 with OCR"]
    xs = [left + (right - left) * i / 2 for i in range(3)]
    body = []
    for v in (0, 0.2, 0.4, 0.6):
        body.append(f'<line x1="{left}" y1="{ys(v):.1f}" x2="{right}" y2="{ys(v):.1f}" stroke="{t["grid"]}"/>')
        body.append(text(left - 8, ys(v) + 4, f"{v:.1f}", t["muted"], 11, 400, "end"))
    for x, label in zip(xs, labels):
        body.append(text(x, top + height + 38, label, t["muted"], 12, 400, "middle"))
    ends = spread([ys(v) + 4 for r in rows for v in (r["values"][-1], r["rival"])], 16)
    for i, r in enumerate(rows):
        c = colors[i % len(colors)]
        pts = " ".join(f"{x:.1f},{ys(v):.1f}" for x, v in zip(xs, r["values"]))
        body.append(f'<line x1="{left}" y1="{ys(r["rival"]):.1f}" x2="{right}" y2="{ys(r["rival"]):.1f}" stroke="{c}" stroke-width="1.5" stroke-dasharray="5 5"/>')
        body.append(f'<polyline points="{pts}" fill="none" stroke="{c}" stroke-width="3" stroke-linejoin="round" stroke-linecap="round"/>')
        for x, v in zip(xs, r["values"]):
            body.append(f'<circle cx="{x:.1f}" cy="{ys(v):.1f}" r="4.5" fill="{c}"/>')
            # 第二条线的数字放在点下面，低分处两条线挨得很近
            body.append(text(x, ys(v) + (20 if i else -11), f"{v:.3f}", t["ink"], 12, 600, "middle"))
        body.append(text(right + 10, ends[2 * i], f"PageSift, {r['name'].lower()}", t["ink"], 12, 600))
        body.append(text(right + 10, ends[2 * i + 1], f"Docling (GPU) {r['rival']:.3f}", t["muted"], 12))
    return frame("Scanned PDFs: olmOCR score by version", "134 scanned pages. Higher is better. Dashed lines are Docling on a GPU.", body, top + height + 50, t)


def main():
    def load(path):
        out = {}
        for line in Path(path).read_text(encoding="utf-8").splitlines():
            k, _, v = line.partition("\t")
            try:
                out[k] = float(v)
            except ValueError:
                pass
        return out

    data = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    metrics = load(sys.argv[2])
    out = Path(sys.argv[3])
    out.mkdir(parents=True, exist_ok=True)
    here = Path(__file__).parent
    card = tomllib.loads((here / "scorecard.toml").read_text(encoding="utf-8"))
    rows = shown(data["rows"])
    mem = memory_rows(metrics, card)
    Path(sys.argv[1]).with_name("memory.json").write_text(json.dumps(mem, ensure_ascii=False, indent=1), encoding="utf-8")
    panels = {}
    for s in card.get("scale", []):
        if "psift_1w" in s:
            s = {**s, "w1": metrics.get(s["psift_1w"]), "w16": metrics.get(s["psift_16w"])}
        if s["w1"] and s["w16"]:
            panels.setdefault(s["panel"], []).append(s)
    old_path = Path(sys.argv[2]).parents[1] / "old" / "metrics.tsv"
    old = load(old_path) if old_path.exists() else {}
    history = load(here / card["versions"]["history"])
    versions = []
    for r in card["versions"]["series"]:
        values = [history.get(r["base"]), old.get(r["base"]), metrics.get(r["ocr"])]
        if all(v is not None for v in values):
            versions.append({**r, "values": values})
    charts = (("scaling", lambda t, c: scaling(panels, t, c)), ("versions", lambda t, c: version_chart(versions, t, c)),
              ("memory", lambda t, c: memory(mem, t)), ("quality", lambda t, c: quality(rows, t)))
    for name, draw in charts:
        for theme, t in THEMES.items():
            svg = draw(t, PALETTE[theme])
            if svg:
                path = out / f"perf-{name}-{theme}.svg"
                path.write_text(svg, encoding="utf-8")
                print(path)


main()

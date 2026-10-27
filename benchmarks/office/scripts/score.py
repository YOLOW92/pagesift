#!/usr/bin/env python3
"""结构保留率评分（越高越好）。规则在实现之前定稿，之后不改（2026-10-27）。
输入：真值目录（truth/<集>/*.json）、某个工具的输出目录（<文件名>.md）。
对每类真值元素判断输出里有没有保住：
- headings：输出里有一行 Markdown 标题（#…）文字与之相同（规范化、不分大小写）；级别另算（只看 docx）。
- tables：输出里有一张管道表，列数相同、行数差不超过 max(1, 10%)（按原样或去掉末尾全空的行列后，任一相符即可），且抽样单元格都在这张表里。
- cells：每个抽样单元格单独算，出现在任何一张表的某个单元格里（相等）即可。
- lists：真值列表项的前 60 字出现在输出某个列表项（- * + 1. 1)）里。
- images：输出某个 ![替代文字] 包含真值的替代文字。
- footnotes / notes：前 60 字出现在输出全文里。
- order：按顺序在全文里找 order 里的标题文字，找到且位置在上一个之后的比例。
另报正文召回（text 的前 60 字出现在全文里的比例，越高越好）。
规范化：NFKC、去掉 Markdown 记号（* _ ` 反斜杠转义、链接写法、<br>）、空白合一、不分大小写。
用法：score.py <truth 目录> <输出目录> [--json 结果文件]"""
import html, json, os, re, sys, unicodedata, glob
def norm(s):
    s = unicodedata.normalize("NFKC", s)
    s = re.sub(r"\[\^([^\]\s]+)\](?!:)", r"\1", s)  # 脚注引用 [^1] 显示为 1（2026-10-27 补，见 REVIEW.md）
    s = re.sub(r"!\[([^\]]*)\]\([^)]*\)", r"\1", s)
    s = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", s)
    s = re.sub(r"<br\s*/?>", " ", s, flags=re.I)
    s = re.sub(r"\\\n", " ", s)  # 行尾反斜杠的硬换行和 <br> 一样当空格（2026-10-27 补，见 REVIEW.md）
    s = re.sub(r"\\([\\`*_{}\[\]()#+\-.!|<>~^])", r"\1", s)
    s = re.sub(r"[*_`]+", "", s)
    return re.sub(r"\s+", " ", s).strip().casefold()
def split_row(line):
    line = line.strip()
    if line.startswith("|"): line = line[1:]
    if line.endswith("|") and not line.endswith("\\|"): line = line[:-1]
    return [norm(c) for c in re.split(r"(?<!\\)\|", line)]
def parse(md):
    heads, tables, items, alts = [], [], [], []
    lines = md.split("\n"); i = 0
    while i < len(lines):
        l = lines[i]
        m = re.match(r"\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$", l)
        if m: heads.append((len(m.group(1)), norm(m.group(2))))
        if l.lstrip().startswith("|") and i + 1 < len(lines) and re.match(r"\s*\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$", lines[i + 1]):
            rows = [split_row(l)]; i += 2
            # 表格里的图片替代文字也算（2026-10-27 补，见 REVIEW.md）
            alts += [norm(a) for a in re.findall(r"!\[([^\]]*)\]", l)]
            while i < len(lines) and lines[i].lstrip().startswith("|"):
                alts += [norm(a) for a in re.findall(r"!\[([^\]]*)\]", lines[i])]
                rows.append(split_row(lines[i])); i += 1
            # 原样的尺寸和去掉末尾全空行列后的尺寸都记下，比较时任一相符即可（2026-10-27 修正，见 REVIEW.md）
            raw = (len(rows), max(map(len, rows)) if rows else 0)
            while rows and not any(rows[-1]): rows.pop()
            ncol = max(map(len, rows)) if rows else 0
            while ncol and all((len(r) < ncol or not r[ncol - 1]) for r in rows): ncol -= 1
            tables.append((rows, ncol, raw)); continue
        m = re.match(r"\s*(?:[-*+]|\d+[.)])\s+(.*)", l)
        if m and not re.match(r"\s*[-*_]{3,}\s*$", l):
            # 列表项的续行和续段（缩进到列表项文字位置的行，中间可隔空行）仍属于这一项，
            # 遇到下一个列表项、表格或不缩进的行为止（CommonMark 的列表项规则；2026-10-27 补，见 REVIEW.md）
            col = m.start(1); text = m.group(1); j = i + 1
            while j < len(lines):
                nxt = lines[j]
                if not nxt.strip(): j += 1; continue
                ind = len(nxt) - len(nxt.lstrip())
                lazy = text.endswith("\\") and j == i + 1  # 硬换行之后紧跟的一行
                if (ind < col and not lazy) or re.match(r"\s*(?:[-*+]|\d+[.)])\s", nxt) or nxt.lstrip().startswith("|"): break
                alts += [norm(a) for a in re.findall(r"!\[([^\]]*)\]", nxt)]
                text = text.rstrip("\\") + " " + nxt.strip(); i = j; j += 1
            items.append(norm(text))
        alts += [norm(a) for a in re.findall(r"!\[([^\]]*)\]", l)]
        i += 1
    # HTML 写法的图片（pandoc 的 HTML 表格里）也算（2026-10-27 补，见 REVIEW.md）
    alts += [norm(html.unescape(a)) for a in re.findall(r"<img\b[^>]*\balt=\"([^\"]*)\"", md, flags=re.I)]
    return heads, tables, items, alts
def score_file(t, md):
    heads, tables, items, alts = parse(md)
    full = norm(md); hset = {h for _, h in heads}; res = {}
    def put(k, ok): a = res.setdefault(k, [0, 0]); a[0] += bool(ok); a[1] += 1
    for lvl, h in t["headings"]:
        ok = norm(h) in hset; put("headings", ok)
        if t["format"] == "docx" and lvl > 0 and ok: put("docx_level", any(x == norm(h) and l == min(lvl, 6) for l, x in heads))  # Markdown 只有 6 级（2026-10-27 补）
    allcells = {c for rows, _, _ in tables for r in rows for c in r}
    for tb in t["tables"]:
        want = [norm(c) for c in tb["cells"]]
        for c in want: put("cells", c in allcells)
        R, C = tb["rows"], tb["cols"]
        fits = lambda r, c: c == C and abs(r - R) <= max(1, R // 10)
        put("tables", any((fits(len(rows), n) or fits(*raw)) and all(c in {x for r in rows for x in r} for c in want) for rows, n, raw in tables))
    for x in t["lists"]: put("lists", any(norm(x)[:60] in it for it in items))
    for x in t["images"]: put("images", any(norm(x) in a for a in alts))
    for k in ("footnotes", "notes"):
        for x in t[k]: put(k, norm(x)[:60] in full)
    pos = 0
    for x in t["order"]:
        p = full.find(norm(x), pos); put("order", p >= 0)
        if p >= 0: pos = p + 1
    for x in t["text"]: put("text", norm(x)[:60] in full)
    return res
STRUCT = ["headings", "tables", "cells", "lists", "images", "footnotes", "notes", "order"]
truth, out = sys.argv[1], sys.argv[2]
per_fmt = {}; files = {}
for f in sorted(glob.glob(os.path.join(truth, "*.json"))):
    t = json.load(open(f)); p = os.path.join(out, t["file"] + ".md")
    md = open(p, encoding="utf-8", errors="replace").read() if os.path.exists(p) else ""
    r = score_file(t, md); files[t["file"]] = r
    agg = per_fmt.setdefault(t["format"], {})
    for k, (a, b) in r.items(): x = agg.setdefault(k, [0, 0]); x[0] += a; x[1] += b
def rate(d, keys): a = sum(d.get(k, [0, 0])[0] for k in keys); b = sum(d.get(k, [0, 0])[1] for k in keys); return a, b
total = {}
for d in per_fmt.values():
    for k, (a, b) in d.items(): x = total.setdefault(k, [0, 0]); x[0] += a; x[1] += b
print(f"{'格式':6} 结构保留率↑      正文召回↑   " + " ".join(f"{k:>10}" for k in STRUCT))
for fmt, d in list(per_fmt.items()) + [("合计", total)]:
    a, b = rate(d, STRUCT); ta, tb = rate(d, ["text"])
    print(f"{fmt:6} {a:4}/{b:<4} {a / max(b, 1):.3f}  {ta / max(tb, 1):.3f}      " + " ".join(f"{d[k][0]:>4}/{d[k][1]:<5}" if k in d else f"{'-':>10}" for k in STRUCT))
if "--json" in sys.argv:
    json.dump({"per_format": per_fmt, "total": total, "files": files}, open(sys.argv[sys.argv.index("--json") + 1], "w"), ensure_ascii=False, indent=1)

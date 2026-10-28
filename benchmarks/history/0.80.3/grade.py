#!/usr/bin/env python3
"""按 URL + 标题给聚焦爬取的结果打真值标签（U 国际生专属 / M 一般招生 / X 无用），两个版本用同一套规则。
用法: grade.py <结果目录>...   每个目录下是 <站名>/index.jsonl"""
import json, re, sys, statistics
from pathlib import Path

INTL = re.compile(r"international|overseas|foreign|global|abroad|non-?eu|exchange", re.I)
ADM = re.compile(r"admission|apply|applicant|application|tuition|fees?\b|scholarship|funding|bursar|visa|immigration|undergraduate|postgraduate|graduate|study|studies|programme|program\b|programs|course|degree|enrol|entry-?requirement|prospective|future-?student|how-to-apply", re.I)
VISA = re.compile(r"\bvisa|immigration|student-?permit|scholarship", re.I)
NOISE = re.compile(r"/news|/press|/blog|/events?/|/about|/research|/alumni|/giving|/donat|/careers|/jobs|/privacy|/accessib|/terms|/contact|/sitemap|/login|/library|/museum|/sport|/202\d/", re.I)

def kind(url, title, status):
    if status and status >= 400:
        return "X"
    text = f"{url} {title or ''}"
    # 标题直说是国际生（或国际学生学者服务中心）的页面
    if re.search(r"international\s+(students?|scholars?|student\s+services|cent(er|re)|office|admissions?)", title or "", re.I):
        return "U"
    if NOISE.search(url) and not INTL.search(url):
        return "X"
    if INTL.search(text) and (ADM.search(text) or VISA.search(text)):
        return "U"
    # 大学站上路径里的 international/overseas 几乎总是国际生专区（新闻、研究路径已经先排除）
    if re.search(r"international|overseas", url, re.I) and not re.search(r"research|partner|office-?of|global-?engagement", url, re.I):
        return "U"
    if VISA.search(text) and "student" in text.lower():
        return "U"
    if ADM.search(text):
        return "M"
    return "X"

def main(dirs):
    for d in dirs:
        d = Path(d)
        rows = []
        for idx in sorted(d.glob("*/index.jsonl")):
            site = idx.parent.name
            c = {"U": 0, "M": 0, "X": 0}
            n = 0
            for line in idx.read_text().splitlines():
                try:
                    r = json.loads(line)
                except json.JSONDecodeError:
                    continue
                url = r.get("url") or r.get("sourceUrl") or ""
                if not url:
                    continue
                n += 1
                c[kind(url, r.get("title"), r.get("status"))] += 1
            err = (d / f"{site}.err").read_text().strip().splitlines()
            tail = err[-1] if err else ""
            rows.append((site, c["U"], c["M"], c["X"], n, tail))
        print(f"== {d}")
        print(f"{'site':<12}{'U':>4}{'M':>4}{'X':>4}{'n':>4}  note")
        for s, u, m, x, n, t in rows:
            print(f"{s:<12}{u:>4}{m:>4}{x:>4}{n:>4}  {t[:70]}")
        pages = sum(r[4] for r in rows); u = sum(r[1] for r in rows); m = sum(r[2] for r in rows)
        fetched = [r for r in rows if r[4] > 0]
        zero = sum(1 for r in fetched if r[1] + r[2] == 0)
        med = statistics.median([r[1] + r[2] for r in fetched]) if fetched else 0
        print(f"sites {len(rows)} (fetched {len(fetched)}): U+M {u+m}/{pages} = {100*(u+m)/max(pages,1):.1f}%, U {100*u/max(pages,1):.1f}%, median U+M {med}, zero-score {zero}")

main(sys.argv[1:])

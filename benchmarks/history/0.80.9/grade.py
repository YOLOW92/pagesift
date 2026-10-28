#!/usr/bin/env python3
"""按 URL + 标题给聚焦爬取的结果打标签，两个版本用同一套规则。
U = 正中查询（国际生专页、研究生申请要求、学生住宿、计算机系的人和研究、图书馆开放和借阅），
M = 相关但泛（一般招生、学生服务、工程学院、图书馆首页），X = 无用（新闻、活动、捐赠、登录……）。
这是粗的代理指标，不等于人读一遍；它的用处是两个版本同口径比较。
用法: grade.py <版本A目录> <版本B目录>      目录下是 <主题>/<站名>/index.jsonl
      grade.py --list <目录> <主题>           列出每页的标签（排查用）"""
import json, re, statistics, sys
from pathlib import Path

I = re.I
NOISE = re.compile(r"/news|/press|/blog|/stories|/events?/|/alumni|/giving|/donat|/careers|/jobs|/vacanc|"
                   r"/privacy|/accessib|/terms|/contact|/sitemap|/login|/sso|/202\d/|/podcast|/magazine", I)

def intl(u, t):
    INTL = re.compile(r"international|overseas|foreign|global|abroad|non-?eu|exchange", I)
    ADM = re.compile(r"admission|apply|applicant|application|tuition|fees?\b|scholarship|funding|bursar|visa|"
                     r"immigration|undergraduate|postgraduate|graduate|study|studies|programme|program\b|programs|"
                     r"course|degree|enrol|entry-?requirement|prospective|future-?student|how-to-apply", I)
    text = f"{u} {t}"
    if re.search(r"international\s+(students?|scholars?|student\s+services|cent(er|re)|office|admissions?)", t, I):
        return "U"
    if re.search(r"/about|/research|/library|/museum|/sport", u, I) and not INTL.search(u):
        return "X"
    if INTL.search(text) and (ADM.search(text) or re.search(r"\bvisa|immigration|scholarship", text, I)):
        return "U"
    if re.search(r"international|overseas", u, I) and not re.search(r"research|partner|office-?of|global-?engagement", u, I):
        return "U"
    return "M" if ADM.search(text) else "X"

def grad(u, t):
    text = f"{u} {t}"
    GRAD = r"graduate|postgrad|ph\.?d|doctoral|doctorate|master|msc\b|grad-?school|gradschool|research-?degree|研究生|大学院|대학원"
    REQ = r"admission|apply|applying|application|requirement|deadline|entry|eligib|prospective|how-to|入学|招生|申请|出願|入試|입학"
    if re.search(GRAD, text, I) and re.search(REQ, text, I):
        return "U"
    if re.search(GRAD + "|" + REQ + r"|programme|program|degree|study|studies", text, I):
        return "M"
    return "X"

def housing(u, t):
    text = f"{u} {t}"
    if re.search(r"housing|accommodation|residen(ce|tial)|dormitor|dorm\b|halls?[-\s]of[-\s]residence|student-?living|"
                 r"on-campus-living|live-on-campus|wohnen|wohnheim|logement|alojamiento|residencia|alloggi|宿舍|住宿|寮|住まい|기숙사", text, I):
        return "U"
    if re.search(r"student-?life|campus-?life|student-?services|new-?students|arriv|living|welfare|support", text, I):
        return "M"
    return "X"

def cs(u, t):
    text = f"{u} {t}"
    CS = (r"computer[-\s]?science|computing|informatics|informatik|informatique|informática|eecs|\bcse\b|"
          r"compsci|/cs/|//cs\.|\bcs\b|computer[-\s]engineering|computer[-\s]and|计算机|情報|컴퓨터")
    if re.search(CS, text, I):
        return "U"
    if re.search(r"engineering|faculty|school-?of|department|research|people|staff|science", text, I) and not NOISE.search(u):
        return "M"
    return "X"

def library(u, t):
    text = f"{u} {t}"
    LIB = r"librar|//lib\.|/lib/|bibliot|図書館|图书馆|도서관"
    if re.search(LIB, text, I) and re.search(r"hours|opening|open|borrow|loan|lend|circulat|renew|visit|access|"
                                             r"using|use-?the|services|ausleih|prêt|préstamo|开放|借阅|開館|貸出", text, I):
        return "U"
    if re.search(LIB, text, I):
        return "M"
    return "X"

TOPICS = {"intl": intl, "grad": grad, "housing": housing, "cs": cs, "library": library}

def kind(topic, url, title, status):
    if status and status >= 400:
        return "X"
    title = title or ""
    # 新闻、活动这类集群：除非 URL 本身就是这个主题（/news/housing-… 也还是新闻）
    if NOISE.search(url):
        return "X"
    return TOPICS[topic](url, title)

def site_rows(d, topic):
    rows = {}
    for idx in sorted((Path(d) / topic).glob("*/index.jsonl")):
        c = {"U": 0, "M": 0, "X": 0}
        for line in idx.read_text().splitlines():
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if r.get("url"):
                c[kind(topic, r["url"], r.get("title"), r.get("status"))] += 1
        if sum(c.values()):
            rows[idx.parent.name] = c
    return rows

def summary(rows, sites):
    n = sum(sum(rows[s].values()) for s in sites)
    u = sum(rows[s]["U"] for s in sites)
    um = sum(rows[s]["U"] + rows[s]["M"] for s in sites)
    med = statistics.median([rows[s]["U"] for s in sites]) if sites else 0
    zero = sum(1 for s in sites if rows[s]["U"] == 0)
    return n, u, um, med, zero

def compare(a, b):
    print(f"{'topic':<8} {'sites':>5}  {'version':<8} {'pages':>5} {'U':>10} {'U+M':>10} {'median U':>9} {'U=0 sites':>9}")
    tot = {a: [0, 0, 0], b: [0, 0, 0]}
    for topic in TOPICS:
        ra, rb = site_rows(a, topic), site_rows(b, topic)
        both = sorted(set(ra) & set(rb))
        for name, rows in ((a, ra), (b, rb)):
            n, u, um, med, zero = summary(rows, both)
            tot[name][0] += n; tot[name][1] += u; tot[name][2] += um
            pct = lambda x: f"{x:>4} {100 * x / n:4.1f}%" if n else "-"
            print(f"{topic:<8} {len(both):>5}  {Path(name).name:<8} {n:>5} {pct(u):>10} {pct(um):>10} {med:>9} {zero:>9}")
    for name in (a, b):
        n, u, um = tot[name]
        print(f"{'all':<8} {'':>5}  {Path(name).name:<8} {n:>5} {u:>4} {100*u/n:4.1f}% {um:>4} {100*um/n:4.1f}%")

def listing(d, topic):
    for idx in sorted((Path(d) / topic).glob("*/index.jsonl")):
        print(f"== {idx.parent.name}")
        for line in idx.read_text().splitlines():
            r = json.loads(line)
            print(f"  {kind(topic, r['url'], r.get('title'), r.get('status'))} {r.get('score') or '':<6} {r['url'][:90]}  | {(r.get('title') or '')[:50]}")

if __name__ == "__main__":
    if sys.argv[1] == "--list":
        listing(sys.argv[2], sys.argv[3])
    else:
        compare(sys.argv[1], sys.argv[2])

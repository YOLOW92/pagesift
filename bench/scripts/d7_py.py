#!/usr/bin/env python3
"""D7 结构化数据竞品适配器：从 HTML 抽 title/author/published。
用法: d7_py.py --tool extruct|bs4lxml --manifest m.jsonl --out o.jsonl"""
import argparse, json, re, sys

def from_extruct(html, url):
    import extruct
    data = extruct.extract(html, base_url=url, syntaxes=["json-ld", "microdata", "opengraph"])
    title = author = published = None
    def walk(o):
        nonlocal title, author, published
        if isinstance(o, dict):
            t = o.get("@type") or o.get("type")
            if isinstance(t, list): t = t[0] if t else None
            if t in ("Article","NewsArticle","BlogPosting","WebPage","TechArticle","Report") or True:
                if title is None and isinstance(o.get("headline"), str): title = o["headline"]
                if title is None and isinstance(o.get("name"), str): title = o["name"]
                a = o.get("author")
                if author is None:
                    if isinstance(a, str): author = a
                    elif isinstance(a, dict): author = a.get("name")
                    elif isinstance(a, list) and a:
                        author = a[0].get("name") if isinstance(a[0], dict) else str(a[0])
                for k in ("datePublished","dateCreated","dateModified"):
                    if published is None and isinstance(o.get(k), str): published = o[k]
            for v in o.values(): walk(v)
        elif isinstance(o, list):
            for v in o: walk(v)
    walk(data.get("json-ld"))
    if title is None:
        for og in data.get("opengraph") or []:
            for p in (og.get("properties") or []):
                if p[0] == "og:title" and title is None: title = p[1]
    return title, author, published

def from_bs4(html, url):
    from bs4 import BeautifulSoup
    s = BeautifulSoup(html, "lxml")
    def meta(*names):
        for n in names:
            m = s.find("meta", attrs={"property": n}) or s.find("meta", attrs={"name": n})
            if m and m.get("content"): return m["content"].strip()
        return None
    title = meta("og:title","twitter:title") or (s.title.get_text(strip=True) if s.title else None)
    author = meta("author","article:author","og:article:author")
    published = meta("article:published_time","datePublished","pubdate","publish-date")
    return title, author, published

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tool", required=True)
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    fn = {"extruct": from_extruct, "bs4lxml": from_bs4}[a.tool]
    n = err = 0
    with open(a.out, "w", encoding="utf-8") as o:
        for line in open(a.manifest, encoding="utf-8"):
            if not line.strip(): continue
            r = json.loads(line)
            try:
                html = open(r["path"], encoding="utf-8", errors="replace").read()
                t, au, pu = fn(html, r.get("url") or "")
            except Exception:
                t = au = pu = None; err += 1
            o.write(json.dumps({"id": r["id"], "title": t, "author": au, "published": pu},
                               ensure_ascii=False) + "\n")
            n += 1
    print(json.dumps({"tool": a.tool, "n": n, "errors": err}), file=sys.stderr)

if __name__ == "__main__":
    main()

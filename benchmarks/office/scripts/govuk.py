#!/usr/bin/env python3
"""从 GOV.UK（开放政府许可 OGL v3）按查询词挑附件。规则写死：
按搜索结果顺序逐个出版物看附件，取第一个指定类型、大小在 [min_kb, max_kb] 的文件；
每个出版物最多一份，凑够 n 份为止。每秒最多 1 个请求，UA 写仓库地址。
用法：govuk.py <ext> <查询词> <n> <输出目录> [max_kb] [min_kb]"""
import json, os, sys, time, urllib.parse, urllib.request, hashlib
UA = "pagesift-eval (+https://github.com/YOLOW92/pagesift)"
ext, q, n, out = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
max_kb = int(sys.argv[5]) if len(sys.argv) > 5 else 3000
min_kb = int(sys.argv[6]) if len(sys.argv) > 6 else 15
os.makedirs(out, exist_ok=True)
def get(url):
    time.sleep(1)
    return urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=60).read()
got = 0; start = 0
log = open(os.path.join(out, "sources.tsv"), "a")
while got < n and start < 400:
    s = json.loads(get("https://www.gov.uk/api/search.json?" + urllib.parse.urlencode({"q": q, "count": 20, "start": start, "fields": "link"})))
    start += 20
    for r in s["results"]:
        if got >= n: break
        try: c = json.loads(get("https://www.gov.uk/api/content" + r["link"]))
        except Exception: continue
        for a in (c.get("details") or {}).get("attachments") or []:
            u = a.get("url") or ""; size = (a.get("file_size") or 0) // 1024
            if not u.lower().endswith("." + ext) or not (min_kb <= size <= max_kb): continue
            data = get(u); name = os.path.basename(urllib.parse.urlparse(u).path)
            open(os.path.join(out, name), "wb").write(data)
            log.write(f"{name}\t{u}\tOGL-3.0 (GOV.UK {r['link']})\t{hashlib.sha256(data).hexdigest()}\n"); log.flush()
            got += 1; print(name, size, "KB"); break
print("取到", got)

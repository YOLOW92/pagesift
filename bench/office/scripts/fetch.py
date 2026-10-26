#!/usr/bin/env python3
"""按清单下载语料并记录来源、许可、sha256。清单每行：<输出目录> <URL> <许可>。每秒 1 个请求。"""
import hashlib, os, sys, time, urllib.parse, urllib.request
UA = "pagesift-eval (+https://github.com/YOLOW92/pagesift)"
for line in open(sys.argv[1]):
    if not line.strip() or line.startswith("#"): continue
    out, url, lic = line.split(None, 2)
    os.makedirs(out, exist_ok=True)
    name = urllib.parse.unquote(os.path.basename(urllib.parse.urlparse(url).path))
    if url.startswith("https://www.gutenberg.org/ebooks/"): name = "gutenberg-" + name.replace(".epub3.images", ".epub")
    path = os.path.join(out, name)
    if os.path.exists(path): continue
    time.sleep(1)
    data = urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=120).read()
    open(path, "wb").write(data)
    with open(os.path.join(out, "sources.tsv"), "a") as f:
        f.write(f"{name}\t{url}\t{lic.strip()}\t{hashlib.sha256(data).hexdigest()}\n")
    print(path, len(data) // 1024, "KB")

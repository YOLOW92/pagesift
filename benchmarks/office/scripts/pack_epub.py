#!/usr/bin/env python3
"""把 IDPF epub3-samples 的解包目录按 EPUB OCF 规范打包：mimetype 第一项、不压缩。
用法：pack_epub.py <样本目录> <输出 .epub>"""
import os, sys, zipfile
src, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w") as z:
    z.write(os.path.join(src, "mimetype"), "mimetype", compress_type=zipfile.ZIP_STORED)
    for root, _, files in sorted(os.walk(src)):
        for f in sorted(files):
            p = os.path.join(root, f); a = os.path.relpath(p, src)
            if a != "mimetype": z.write(p, a, compress_type=zipfile.ZIP_DEFLATED)

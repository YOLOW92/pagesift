#!/usr/bin/env python3
"""用 MarkItDown 转换一个集合，每个文件写 <out>/<文件名>.md；失败的写空文件并记错误。
用法：~/.venvs/markitdown/bin/python run_markitdown.py <集目录> <输出目录>"""
import os, sys, glob, json, importlib.metadata as m
from markitdown import MarkItDown
src, out = sys.argv[1], sys.argv[2]; os.makedirs(out, exist_ok=True)
md = MarkItDown(); errs = {}
for p in sorted(glob.glob(os.path.join(src, "*", "*.*"))):
    if p.endswith(".tsv"): continue
    try: text = md.convert(p).markdown
    except Exception as e: text = ""; errs[os.path.basename(p)] = repr(e)[:300]
    open(os.path.join(out, os.path.basename(p) + ".md"), "w").write(text)
json.dump({"version": m.version("markitdown"), "errors": errs}, open(os.path.join(out, "_run.json"), "w"), indent=1)
print("MarkItDown", m.version("markitdown"), "失败", len(errs), list(errs)[:5])

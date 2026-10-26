#!/usr/bin/env python3
"""人工核对用：列出 docx 段落的样式名、是否带编号（numPr）、大纲级别和前 70 个字。"""
import sys
from docx import Document
d = Document(sys.argv[1]); n = int(sys.argv[2]) if len(sys.argv) > 2 else 80
for p in d.paragraphs[:n]:
    ppr = p._p.pPr
    num = ppr is not None and ppr.numPr is not None
    if p.text.strip(): print(f"{p.style.name[:22]:22} {'N' if num else ' '} {p.text.strip()[:70]}")

#!/usr/bin/env python3
"""MarkItDown 进程内耗时：每个文件先转一次预热，再转 N 次取中位数（毫秒，越低越好）。输出 TSV 同 office_bench。
用法：~/.venvs/markitdown/bin/python time_markitdown.py <次数> <文件...>"""
import sys, time, statistics, warnings, os
warnings.filterwarnings("ignore")
from markitdown import MarkItDown
md = MarkItDown(); runs = int(sys.argv[1])
for p in sys.argv[2:]:
    try: size = len(md.convert(p).markdown.encode())
    except Exception: print(f"{os.path.basename(p)}\tNA\t0"); continue
    ts = []
    for _ in range(runs):
        t = time.perf_counter(); md.convert(p); ts.append((time.perf_counter() - t) * 1e3)
    print(f"{os.path.basename(p)}\t{statistics.median(ts):.3f}\t{size}")

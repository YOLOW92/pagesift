#!/usr/bin/env python3
"""稳健版速度内存闸门（越低越好）：每个文件交替跑 R 轮，每轮每个二进制 pdf_one 5 次取最快；
耗时取各轮最小值，内存取各轮峰值 RSS 的中位数。用法：gate_rounds.py <输出.tsv> <轮数> 名字=二进制 ... -- pdf ..."""
import os, statistics as st, subprocess, sys
out, rounds = sys.argv[1], int(sys.argv[2])
i = sys.argv.index("--")
bins = [a.split("=", 1) for a in sys.argv[3:i]]
pdfs = sys.argv[i + 1:]
res = {}
for r in range(rounds):
    for f in pdfs:
        for name, b in bins:
            p = subprocess.run(["/usr/bin/time", "-f", "RSS %M", b, f, "5"], capture_output=True, text=True)
            ms = float(p.stdout.split("\t")[1])
            kb = int([l for l in p.stderr.splitlines() if l.startswith("RSS")][-1].split()[1])
            res.setdefault((f, name), []).append((ms, kb / 1024))
with open(out, "w") as fh:
    fh.write("文件" + "".join(f"\t{n}_ms\t{n}_MB" for n, _ in bins) + "\n")
    for f in pdfs:
        fh.write(os.path.basename(f)[:-4])
        for n, _ in bins:
            v = res[(f, n)]
            fh.write(f"\t{min(x[0] for x in v):.2f}\t{st.median(x[1] for x in v):.1f}")
        fh.write("\n")

#!/usr/bin/env python3
"""cmp.py <结果目录>... [--topics=a,b]：每个方向和全部，只比所有版本都抓到页面的任务。
列：总页数、每站中位数和最多页数、U 页数和占比、U+M 占比、一个 U 都没有的站数；最后是停止原因。"""
import sys, re, json, statistics
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent)); import grade
args=[a for a in sys.argv[1:] if not a.startswith('--')]
topics=list(grade.TOPICS)
for a in sys.argv[1:]:
    if a.startswith('--topics='): topics=a.split('=')[1].split(',')
def load(d):
    out={}
    for t in topics:
        for idx in sorted((Path(d)/t).glob('*/index.jsonl')):
            rows=[json.loads(l) for l in idx.read_text().splitlines() if l.strip()]
            lab=[grade.kind(t,r['url'],r.get('title'),r.get('status')) for r in rows if r.get('url')]
            err=Path(str(idx.parent)+'.err'); m=re.search(r'^stopped: (\S+)', err.read_text(errors='replace'), re.M) if err.exists() else None
            if lab: out[(t,idx.parent.name)]=(lab, m.group(1) if m else None)
    return out
runs=[(Path(d).name, load(d)) for d in args]
common=set.intersection(*[set(r) for _,r in runs])
def row(name, r, ks):
    pages=[len(r[k][0]) for k in ks]; n=sum(pages)
    U=sum(r[k][0].count('U') for k in ks); UM=sum(r[k][0].count('U')+r[k][0].count('M') for k in ks)
    z=sum(1 for k in ks if r[k][0].count('U')==0)
    return f"{name:<12} {n:>6} {statistics.median(pages):>6} {max(pages):>5} {U:>6} {100*U/n:>6.1f}% {100*UM/n:>6.1f}% {z:>5}"
print(f"tasks reached by every run: {len(common)}")
hdr=f"{'run':<12} {'pages':>6} {'median':>6} {'max':>5} {'U':>6} {'U%':>7} {'U+M%':>7} {'U=0':>5}"
for t in topics+['all']:
    ks=[k for k in common if t=='all' or k[0]==t]
    if not ks: continue
    print(f"\n[{t}] {len(ks)} tasks\n{hdr}")
    for name,r in runs: print(row(name,r,ks))
print()
for name,r in runs:
    stops={}
    for k in common: stops[r[k][1]]=stops.get(r[k][1],0)+1
    print(name, dict(sorted(stops.items(), key=lambda x: -x[1])))

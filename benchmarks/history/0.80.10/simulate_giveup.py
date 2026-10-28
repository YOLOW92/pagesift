"""blind.py <结果目录>：在上限 100 的结果上模拟"还没有收获时，连续 B 页盲探（分数不高于栏目保底分）
而且无关就停"，B 取不同值时省下的页数和丢掉的 U 页。"""
import sys, json
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent)); import grade
d = Path(sys.argv[1]); FLOOR=0.2701
tasks=[]
for t in grade.TOPICS:
    for idx in sorted((d/t).glob('*/index.jsonl')):
        rows=[json.loads(l) for l in idx.read_text().splitlines() if l.strip()]
        if not rows: continue
        U=[grade.kind(t,r['url'],r.get('title'),r.get('status'))=='U' for r in rows]
        tasks.append((t, idx.parent.name, rows, U))
totalU=sum(sum(U) for *_,U in tasks); total=sum(len(r) for _,_,r,_ in tasks)
for B in (3,5,8,10,15,20):
    saved=lost=0; cutT=[]
    for t,s,rows,U in tasks:
        found=False; run=0; cut=None
        for i,r in enumerate(rows):
            if (r.get('relevance') or 0)>=0.3: found=True
            if i==0: continue
            blind=(r.get('score') or 0)<=FLOOR
            run = run+1 if blind else 0
            if not found and run>=B: cut=i+1; break
        if cut:
            saved+=len(rows)-cut; l=sum(U[cut:]); lost+=l
            if l: cutT.append((t,s,cut,l))
    print(f"B={B:>2}: saved {saved:>5} ({100*saved/total:4.1f}%) U lost {lost:>4} ({100*lost/totalU:4.1f}%)  {cutT[:8]}")

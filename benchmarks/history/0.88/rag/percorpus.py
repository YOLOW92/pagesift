import json,sys,os
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
from rag_score import per_question
data=sys.argv[1]; half=sys.argv[2]
res={p:per_question(data,json.load(open(p))) for p in sys.argv[3:]}
cs=['chatlogs','finance','pubmed','state_of_the_union','wikitexts']
print('%-28s'%'', ' '.join('%-23s'%c[:12] for c in cs))
for p,r in res.items():
    row=[]
    for c in cs:
        idx=[i for i,x in enumerate(r['corpus']) if x==c and (half=='all' or r['half'][i]==(1 if half=='hold' else 0))]
        row.append(' '.join('%.3f'%(sum(r[f'R{b}'][i] for i in idx)/len(idx)) for b in (250,500,1000)))
    print('%-28s'%os.path.basename(p)[:28], ' '.join('%-23s'%x for x in row))

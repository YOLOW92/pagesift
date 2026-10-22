#!/usr/bin/env python3
"""Rank per-document losses and compute a paired, site-cluster bootstrap interval.

Reads the upstream AEB scorer's per-document TP/FP/FN, preserving its normalization
and metric. Rankings are diagnostic; they are not automatic root-cause findings.
"""
import argparse
import collections
import json
from pathlib import Path
import random
import statistics
from urllib.parse import urlsplit
from tld import get_fld

ROOT=Path(__file__).resolve().parents[2]


def prf(triple):
    tp,fp,fn=triple
    p=1. if fp==fn==0 else (tp/(tp+fp) if tp+fp else 0.)
    r=1. if fp==fn==0 else (tp/(tp+fn) if tp+fn else 0.)
    return p,r,2*p*r/(p+r) if p+r else 0.


def macro(triples):
    ps=[];rs=[]
    for triple in triples:
        tp,fp,fn=triple;p,r,_=prf(triple)
        if tp+fp>0:ps.append(p)
        if tp+fn>0:rs.append(r)
    p=statistics.mean(ps) if ps else 0.;r=statistics.mean(rs) if rs else 0.
    return 2*p*r/(p+r) if p+r else 0.


def main():
    p=argparse.ArgumentParser();p.add_argument('result');p.add_argument('pagesift');p.add_argument('other');p.add_argument('output');a=p.parse_args()
    scores=json.loads(Path(a.result).read_text())
    truth=json.loads((ROOT/'bench/data/fresh/data/aeb/ground-truth.json').read_text())
    keys=list(truth)
    left=scores[a.pagesift]['tp_fp_fns'];right=scores[a.other]['tp_fp_fns']
    assert len(keys)==len(left)==len(right)
    assert abs(macro(left)-scores[a.pagesift]['f1'])<1e-12
    assert abs(macro(right)-scores[a.other]['f1'])<1e-12
    rows=[];groups=collections.defaultdict(list)
    for i,key in enumerate(keys):
        url=truth[key]['url'];site=get_fld(url,fail_silently=True) or urlsplit(url).hostname
        groups[site].append(i)
        lp,lr,lf=prf(left[i]);rp,rr,rf=prf(right[i])
        rows.append({'id':key,'url':url,'site':site,'pagesift_f1':lf,'other_f1':rf,
                     'gap_other_minus_pagesift':rf-lf,'pagesift_precision':lp,'pagesift_recall':lr,
                     'other_precision':rp,'other_recall':rr,
                     'inspect':'recall' if rr>lr else 'precision/extra text'})
    sites=sorted(groups);rng=random.Random(582031231);samples=[]
    for _ in range(2000):
        indices=[i for site in rng.choices(sites,k=len(sites)) for i in groups[site]]
        samples.append(macro([left[i] for i in indices])-macro([right[i] for i in indices]))
    samples.sort()
    result={'pagesift':a.pagesift,'other':a.other,'n':len(rows),'site_clusters':len(sites),
            'metric':'upstream AEB harmonic mean of macro precision and recall',
            'difference_pagesift_minus_other':macro(left)-macro(right),
            'paired_site_bootstrap_95_percentile':[samples[50],samples[1949]],'bootstrap_seed':582031231,
            'bootstrap_repetitions':2000,
            'pagesift_losses':sorted([r for r in rows if r['gap_other_minus_pagesift']>1e-6],key=lambda r:-r['gap_other_minus_pagesift']),
            'all':rows}
    Path(a.output).write_text(json.dumps(result,indent=2,ensure_ascii=False)+'\n')
    print(json.dumps({k:v for k,v in result.items() if k not in ['all','pagesift_losses']},indent=2))
    for row in result['pagesift_losses'][:10]:print(row['id'],round(row['gap_other_minus_pagesift'],4),row['inspect'],row['url'])


if __name__=='__main__': main()

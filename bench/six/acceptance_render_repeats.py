#!/usr/bin/env python3
"""Repeat the same held-out families; count stochastic adaptive outcomes honestly."""
import json,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4'


def main():
    scores=[]
    first=json.loads((OUT/'render-final/summary.json').read_text())
    def record(d):return {name:sum(r['answer_found'] for r in d[name]) for name in ('baseline','candidate','crawlee')}
    scores.append(record(first))
    for i in range(2,6):
        d=OUT/f'render-repeat-{i}'
        with (OUT/f'render-repeat-{i}.log').open('w') as log:
            subprocess.run([sys.executable,'bench/six/holdout_render.py',str(OUT/'protocol-v2.json'),str(d),
                 str(ROOT/'bench/data/six/acceptance/source/target/release/pagesift'),
                 str(ROOT/'bench/data/six/acceptance/source-v2/target/release/pagesift')],stdout=log,stderr=subprocess.STDOUT,check=True,timeout=360)
        scores.append(record(json.loads((d/'summary.json').read_text())))
    result={'families':8,'repetitions':5,'scores':scores,'candidate_all_pass':all(r['candidate']==8 for r in scores),
      'scope':'Repeated observations on eight controlled families, not 40 independent sites. Crawlee default detection is stochastic; no runtime ranking.'}
    (OUT/'render-repeats.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))


if __name__=='__main__':main()

#!/usr/bin/env python3
"""Use the historical five separate-subset calls, not a changed overall weighting."""
import json,os,re,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4/domains-final/pdf'
TOOLS=ROOT/'bench/data/fresh/tools'


def main():
    scores={}
    env=dict(os.environ,PYTHONPATH=str(TOOLS/'olmocr-bench'),PYTHONDONTWRITEBYTECODE='1',PYTHON_CPU_COUNT='8')
    for subset in ('arxiv_math','headers_footers','long_tiny_text','multi_column','table_tests'):
        path=OUT/(subset+'-score.log')
        with path.open('w') as log:
            subprocess.run([str(TOOLS/'py/bin/python'),'-X','cpu_count=8',str(ROOT/'bench/data/six/acceptance/source-v2/bench/pdf/bench_fast.py'),
                '--dir',str(OUT/(subset+'.jsonl')),'--candidate','candidate','--bootstrap_samples','200'],
                cwd=TOOLS/'olmocr-bench',env=env,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=600)
        matches=re.findall(r'Average Score: ([0-9.]+)%',path.read_text())
        if not matches:raise RuntimeError('missing original PDF score')
        scores[subset]=float(matches[0])
    result={'per_subset_percent':scores,'mean_of_five_subsets_percent':sum(scores.values())/5,
        'aggregation':'historical per-subset native benchmark invocations followed by equal mean of five subsets',
        'combined_diagnostic_log':'score.log uses a different baseline weighting; not comparable to the historical overall metric',
        'byte_identical_baseline_candidate_pdfs':1269}
    (OUT/'historical-aggregation.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))


if __name__=='__main__':main()

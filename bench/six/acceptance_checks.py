#!/usr/bin/env python3
"""Serial candidate checks; independent samples and known regressions stay separate."""
import json,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4'
CAND=ROOT/'bench/data/six/acceptance/source-v2/target/release/pagesift'
BEFORE_FIX=ROOT/'bench/data/six/acceptance/source/target/release/pagesift'
BASE=ROOT/'target/release/pagesift'


def main():
    steps=[('render-development',[sys.executable,'bench/six/holdout_render.py',str(OUT/'protocol.json'),str(OUT/'render-regression'),str(BEFORE_FIX),str(CAND)]),
      ('render-independent',[sys.executable,'bench/six/holdout_render.py',str(OUT/'protocol-v2.json'),str(OUT/'render-final'),str(BEFORE_FIX),str(CAND)]),
      ('images-independent',[sys.executable,'bench/six/holdout_images.py',str(ROOT/'bench/data/six/acceptance/data-v2/images.jsonl'),str(OUT/'images-final'),str(BASE),str(CAND)]),
      ('discovery-independent',[sys.executable,'bench/six/holdout_discovery.py',str(OUT/'protocol-v2.json'),str(OUT/'discovery-final'),str(BASE),str(CAND)]),
      ('official-sdk',[str(ROOT/'bench/data/six/venv/bin/python'),'bench/six/sdk.py',str(OUT/'sdk-final'),str(CAND)]),
      ('known-193-replay',[sys.executable,'bench/six/replay_regression.py',str(OUT/'replay-final'),str(BEFORE_FIX),str(CAND)])]
    records=[]
    for name,command in steps:
        with (OUT/(name+'.log')).open('w') as log:
            proc=subprocess.run(command,cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,timeout=360)
        records.append({'name':name,'command':command,'exit':proc.returncode})
        (OUT/'checks.json').write_text(json.dumps(records,indent=2)+'\n')
        if proc.returncode:raise RuntimeError(name+' infrastructure error; inspect log')
    # Quality is evaluated from saved gold scores, independently of process exit.
    gates={key:json.loads((OUT/path/'summary.json').read_text())[field] for key,path,field in
        [('render','render-final','candidate_passes_frozen_threshold'),('images','images-final','candidate_passes'),('discovery','discovery-final','candidate_passes')]}
    (OUT/'quality-gates.json').write_text(json.dumps(gates,indent=2)+'\n');print(json.dumps(gates))


if __name__=='__main__':main()

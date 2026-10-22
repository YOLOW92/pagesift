#!/usr/bin/env python3
"""Five serial native regression gates; no competitor performance ranking."""
import hashlib,json,re,subprocess,sys,time
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
OUT=Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'bench/six/results/round4/perf-final'
EX=ROOT/'bench/data/six/acceptance/source-v2/target/release/examples'


def main():
    OUT.mkdir(parents=True,exist_ok=False);rows=[]
    binaries=[]
    for package in ('pagesift-content','pagesift-extract'):
        candidates=[p.with_suffix('') for p in EX.glob('perf_gate-*.d')
            if f'crates/{package}/examples/perf_gate.rs' in p.read_text() and p.with_suffix('').is_file()]
        if len(candidates)!=1:raise RuntimeError('ambiguous performance gate artifact: '+package+str(candidates))
        binaries.append((package,candidates[0]))
    binaries.append(('crawl',EX/'crawl_gate'))
    for i in range(5):
        log=OUT/f'run-{i+1}.log';text=''
        for name,binary in binaries:
            proc=subprocess.run([str(binary)],text=True,capture_output=True,check=True,timeout=120)
            text+=proc.stdout+proc.stderr
        log.write_text(text)
        assert '1200/1200 pages, 0 errors' in text and 'fail' not in text.lower()
        rows.append({'iteration':i+1,'stdout':text,'loadavg':Path('/proc/loadavg').read_text().strip()})
    (OUT/'summary.json').write_text(json.dumps({'n':5,'all_pass':True,'rows':rows,'binaries':{name:{'path':str(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()} for name,p in binaries},
      'scope':'serial threshold regression gates on frozen clone, background desktop applications not stopped; no cross-tool speed ranking'},indent=2)+'\n')
    print(json.dumps({'n':5,'all_pass':True}))


if __name__=='__main__':main()

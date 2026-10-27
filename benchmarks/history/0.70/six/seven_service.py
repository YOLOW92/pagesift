#!/usr/bin/env python3
"""Complete self-hosted Firecrawl replay of ALL 545 new calibrated pages.
Reuse the established service protocol, keeping transport-host and component
boundaries explicit. Docker caps 6784 MiB/720% + runner 1408 MiB/80% = 8 GiB/8 CPU.
"""
import hashlib,json,sys
from pathlib import Path
import firecrawl_full as full
import seven_corpus as corpus
ROOT=Path(__file__).resolve().parents[4]
DATA=ROOT/'benchmarks/data/six/seven-next/service-input'
OUT=ROOT/'benchmarks/history/0.70/six/results/seven-next/service'

def prepare():
 DATA.mkdir(parents=True,exist_ok=False)
 rows=list(map(json.loads,(corpus.OUT/'manifest.jsonl').open()))
 warm=DATA/'warmup.html';warm.write_text('<article><h1>Image example</h1><p>Readiness only, excluded from all scores.</p></article>')
 (DATA/'fixtures.jsonl').write_text(json.dumps({'id':'image-readiness-only','path':str(warm),'only_main':True})+'\n')
 (DATA/'aeb.jsonl').write_text(''.join(json.dumps(dict(r,only_main=True))+'\n' for r in rows))
 binary=DATA/'rust-target/release/pagesift';binary.parent.mkdir(parents=True);binary.symlink_to(ROOT/'benchmarks/data/six/editorial-target/debug/pagesift')
 protocol={'n':545,'same_html_bytes':True,'URL_context':'same replay /case/<UUID> path; Pagesift accesses Docker host gateway, Firecrawl accesses its extra-host replay name, as in prior established service protocol','metric':'same token multiset F1 diagnostic as seven_corpus, in replay URL context; not mixed with original-URL component scores','scope':'fixed complete self-hosted Firecrawl service, not managed cloud','readiness_excluded':'image-readiness-only','product_binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'docker_images':json.loads((ROOT/'benchmarks/history/0.70/six/locks/firecrawl-runtime.json').read_text()),'docker_memory_caps_mb':6784,'runner_memory_cap_mb':1408,'docker_cpu_quota_percent':720,'runner_cpu_quota_percent':80}
 (DATA/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n')
 print(json.dumps({'prepared':len(rows)}))

def score():
 ids={r['id'] for r in map(json.loads,(corpus.OUT/'manifest.jsonl').open())}
 result={}
 for tool in ['pagesift','firecrawl']:
  raw=list(map(json.loads,(OUT/(tool+'.jsonl')).open()));rows=[r for r in raw if r['id'] in ids]
  assert {r['id'] for r in rows}==ids
  dest=OUT/tool;dest.mkdir(exist_ok=True);path=dest/'results.jsonl';path.write_text(''.join(json.dumps(r)+'\n' for r in rows))
  result[tool]=corpus.score(path)
 (OUT/'scores-summary.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))

if __name__=='__main__':
 if sys.argv[1]=='prepare':prepare()
 elif sys.argv[1]=='run':
  full.DATA=DATA;sys.argv=[sys.argv[0],str(OUT)];full.main()
 else:score()

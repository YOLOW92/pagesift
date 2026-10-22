#!/usr/bin/env python3
"""Descriptive paired hostname bootstrap on the frozen independent-page run."""
import json,random,statistics
from pathlib import Path
from urllib.parse import urlsplit
OUT=Path(__file__).resolve().parents[2]/'bench/six/results/editorial-core/validation'
rows=list(map(json.loads,(OUT/'manifest.jsonl').open()));scores={k:{r['id']:r['f1'] for r in json.loads((OUT/k/'scores.json').read_text())} for k in ['before','after','essence']};groups={}
for r in rows:groups.setdefault(urlsplit(r['url']).hostname or '',[]).append(r['id'])
keys=list(groups);rng=random.Random(20261022);intervals={}
for reference in ['before','essence']:
 samples=[]
 for _ in range(2000):
  ids=[k for _ in keys for k in groups[rng.choice(keys)]];samples.append(statistics.mean(scores['after'][k]-scores[reference][k] for k in ids))
 samples.sort();intervals[reference]={'delta':statistics.mean(scores['after'][k]-scores[reference][k] for k in scores['after']),'paired_hostname_bootstrap_95_percentile':[samples[50],samples[1949]]}
errors=[r['id'] for r in map(json.loads,(OUT/'essence/results.jsonl').open()) if r['status']!='ok'];valid=[k for k in scores['after'] if k not in errors]
summary={'scope':'descriptive post-run uncertainty, not preregistered significance test','seed':20261022,'repetitions':2000,'hostname_groups':len(groups),'comparisons':intervals,'essence_timeout_sensitivity':{'excluded':errors,'n':len(valid),'mean_f1':{t:statistics.mean(scores[t][k] for k in valid) for t in scores}}};(OUT/'uncertainty.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))

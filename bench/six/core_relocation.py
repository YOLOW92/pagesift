#!/usr/bin/env python3
"""Replay business anchors including tag replacement and cross-tag ambiguity."""
import hashlib,json,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
old=ROOT/'bench/six/results/round4/structured-final';previous=ROOT/'bench/six/results/capabilities/verified';rows=[json.loads(l) for l in (previous/'relocation.jsonl').open()];source=(old/'original.html').read_text();sel=rows[0]['selector'];key=sel[1:];tagged=source.replace('<section','<article').replace('</section>','</article>')
for ident,text,gold in [('changed-tag',tagged,'Independent Widget 東京 Cost 73'),('changed-tag-and-selector',tagged.replace('id="'+key+'"','class="new-layout"'),'Independent Widget 東京 Cost 73'),('cross-tag-duplicate',source+tagged,None),('reused-selector-different-tag',source.replace('inventory-A','inventory-B')+tagged.replace('id="'+key+'"','id="moved"'),'Independent Widget 東京 Cost 73')]:
 p=out/(ident+'.html');p.write_text(text);rows.append({'id':ident,'path':str(p),'selector':sel,'gold':gold})
manifest=out/'manifest.jsonl';manifest.write_text(''.join(json.dumps(r)+'\n' for r in rows))
summary={}
for label,binary in [('baseline',Path(sys.argv[2]).resolve()),('candidate',Path(sys.argv[3]).resolve())]:
 output=out/(label+'.jsonl');subprocess.run([str(binary),str(old/'original.html'),sel,str(manifest),str(output)],check=True,timeout=30)
 pred={r['id']:r for r in map(json.loads,output.open())};details=[]
 for r in rows:
  p=pred[r['id']];norm=lambda values:[''.join(s.split()) for s in values];wanted=[] if r['gold'] is None else [r['gold']];details.append(dict(id=r['id'],status=p['status'],correct=norm(p['values'])==norm(wanted)))
 summary[label]={'n':len(rows),'correct':sum(r['correct'] for r in details),'details':details,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
assert summary['candidate']['correct']==len(rows)
assert next(r for r in summary['candidate']['details'] if r['id']=='cross-tag-duplicate')['status']=='ambiguous'
(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({k:{x:y for x,y in v.items() if x!='details'} for k,v in summary.items()}))

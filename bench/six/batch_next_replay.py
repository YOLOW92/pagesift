#!/usr/bin/env python3
"""Post-run reproducibility check: unchanged current binary, all 511 inputs."""
import json,shutil,statistics
import batch_next as driver
primary=driver.OUT;out=primary/'replay';out.mkdir(exist_ok=False)
for name in ['protocol.json','manifest.jsonl']:shutil.copyfile(primary/name,out/name)
driver.OUT=out;driver.run_tool('current')
before={r['id']:r for r in map(json.loads,(primary/'current/results.jsonl').open())};after={r['id']:r for r in map(json.loads,(out/'current/results.jsonl').open())};gold=driver.official.load_ground_truth('test');assert set(before)==set(after)==set(gold)
changes=[];scores=[]
for k in after:
 old=driver.plain(before[k]['markdown']);new=driver.plain(after[k]['markdown']);f=driver.official.word_f1(new,gold[k]['main_content'])[2];scores.append(f)
 if before[k]['markdown']!=after[k]['markdown']:changes.append({'id':k,'before_f1':driver.official.word_f1(old,gold[k]['main_content'])[2],'after_f1':f})
summary={'scope':'post-hoc repeatability check, not replacement of primary results','n':len(after),'markdown_identical':len(after)-len(changes),'changed':changes,'repeat_f1':statistics.mean(scores),'failures':[k for k,r in after.items() if r['status']!='ok']};(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))

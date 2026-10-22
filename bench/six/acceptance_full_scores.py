#!/usr/bin/env python3
"""Physical-newline JSONL parsing: U+2028/U+0085 inside strings are valid JSON."""
import json
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4'


def main():
    manifest=json.loads((OUT/'full-input.json').read_text());gold={r['id']:r for r in manifest['images']}
    expected=set(gold)|{r['id'] for r in manifest['live']}|{manifest['readiness_id']}
    result={}
    for tool in ('pagesift','firecrawl'):
        with (OUT/f'firecrawl-final/{tool}.jsonl').open() as f:rows=[json.loads(line) for line in f if line.strip()]
        by={r['id']:r for r in rows}
        assert len(rows)==len(by)==len(expected) and set(by)==expected
        result[tool]={'n_images':len(gold),'image_field_hits':sum(any(u in by[k]['images'] for u in g['gold_images']) for k,g in gold.items()),
          'markdown_hits':sum(any(u in by[k]['markdown'] for u in g['gold_images']) for k,g in gold.items()),
          'live_snapshot_success':sum(by[r['id']]['status']=='ok' for r in manifest['live']),
          'live_snapshot_n':len(manifest['live']),'readiness_excluded':True,
          'all_responses_verified':all(r['status']=='ok' and r['upstream_status']==200 for r in rows),'jsonl_records':len(rows)}
    (OUT/'firecrawl-image-scores.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
    assert all(r['all_responses_verified'] for r in result.values())
    assert result['pagesift']['image_field_hits']==len(gold)


if __name__=='__main__':main()

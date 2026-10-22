#!/usr/bin/env python3
"""Run the unchanged complete-service protocol with explicit new frozen inputs."""
import hashlib,json,sys
from pathlib import Path
import firecrawl_full as full
ROOT=Path(__file__).resolve().parents[2]
DATA=ROOT/'bench/data/six/acceptance/full-input'


def prepare():
    DATA.mkdir(parents=True,exist_ok=False)
    original=ROOT/'bench/data/six/acceptance/data-v2/images.jsonl'
    rows=list(map(json.loads,original.read_text().splitlines()))
    for row in rows:row['id']='image-'+row['id']
    warm=DATA/'warmup.html';warm.write_text('<article><h1>Image example</h1><p>Service readiness only; excluded from acceptance scores.</p></article>')
    fixtures=[{'id':'image-readiness-only','path':str(warm),'only_main':False}]+rows
    (DATA/'fixtures.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in fixtures))
    live=json.loads((ROOT/'bench/six/results/round4/live-fetch-v2.json').read_text());articles=[]
    from email.message import Message
    for row in live:
        if row['status']!='fetched':continue
        body=Path(row['path']).read_bytes();m=Message();m['content-type']=row['content_type']
        charset=m.get_content_charset() or 'utf-8'
        try:text=body.decode(charset,errors='replace')
        except LookupError:charset='utf-8';text=body.decode(charset,errors='replace')
        p=DATA/f"live-{row['rank']}.html";p.write_text(text)
        articles.append({'id':f"live-{row['rank']}",'path':str(p),'only_main':True,'source_url':row['final_url'],
            'raw_sha256':row['sha256'],'charset':charset,'replacement_characters':text.count('\ufffd'),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()})
    (DATA/'aeb.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in articles))
    link=DATA/'rust-target';link.symlink_to(ROOT/'bench/data/six/acceptance/source-v2/target',target_is_directory=True)
    manifest={'scope':'new image gold plus fresh live snapshots with no content gold; readiness excluded',
        'images':rows,'live':articles,'readiness_id':'image-readiness-only'}
    (ROOT/'bench/six/results/round4/full-input.json').write_text(json.dumps(manifest,indent=2)+'\n')


def main():
    if sys.argv[1]=='prepare':prepare();return
    full.DATA=DATA
    sys.argv=[sys.argv[0],sys.argv[1]]
    full.main()


if __name__=='__main__':main()

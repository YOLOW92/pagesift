#!/usr/bin/env python3
"""Freeze fresh seeds, synthetic acceptance cases and an unreplaced live sample."""
import concurrent.futures,datetime,hashlib,html,json,random,secrets,time,urllib.request,urllib.error,urllib.robotparser,zipfile
from pathlib import Path
from urllib.parse import urlsplit

ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4'
LABEL=__import__('sys').argv[1] if len(__import__('sys').argv)>1 else ''
DATA=ROOT/('bench/data/six/acceptance/data'+LABEL)
UA='pagesift-acceptance/0.12 (+https://github.com/YOLOW92/pagesift)'


def main():
    DATA.mkdir(parents=True,exist_ok=False)
    seed=secrets.randbits(48);rng=random.Random(seed)
    protocol={'master_seed':seed,'created_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
      'markdown_seeds':[rng.randrange(1,2**31) for _ in range(3)],'markdown_docs_per_seed':200,
      'adaptive_families':[], 'acceptance':{'same_family_dynamic_answer':'required, including shared first path segment and query routes',
      'known_domains_regression':'no change versus original baseline outputs; score with existing gold',
      'fresh_live':'fixed sample, fetch failures retained; no quality gold or population accuracy claim',
      'no_speed_ranking':'timings are diagnostics unless all comparison conditions are satisfied'}}
    for kind in ('different-segment','same-segment','query-route','delayed-route','external-script','body-onload','json-data-only','module-script'):
        slug=''.join(rng.choice('abcdefghijklmnopqrstuvwxyz') for _ in range(10))
        protocol['adaptive_families'].append({'kind':kind,'slug':slug,'answer':'ACCEPT_'+secrets.token_hex(8)})
    # Sampling frame is the existing frozen ranking, not a claim of today's ranks.
    ranking=ROOT/'bench/data/fresh/data/tranco.zip'
    with zipfile.ZipFile(ranking) as z:
        names=[n for n in z.namelist() if n.endswith('.csv')]
        lines=z.read(names[0]).decode().splitlines()
    known=set()
    old=ROOT/'bench/data/inputs/manifest.jsonl'
    if old.exists():
        for row in map(json.loads,old.read_text().splitlines()):
            u=row.get('url') or row.get('base_url','')
            known.add(urlsplit(u).hostname)
    frame=[]
    for line in lines:
        n,domain=line.split(',',1)
        if 1000<=int(n)<=200000 and domain not in known:frame.append((int(n),domain))
    sample=rng.sample(frame,12)
    protocol['live_sample']=[{'rank':rank,'domain':domain,'url':'https://'+domain+'/'} for rank,domain in sample]
    protocol['ranking_sha256']=hashlib.sha256(ranking.read_bytes()).hexdigest()
    protocol['nonoverlap_scope']='excludes hostnames in prior bench/data/inputs/manifest.jsonl only; other historical sets may overlap'
    (OUT/('protocol'+LABEL+'.json')).write_text(json.dumps(protocol,indent=2)+'\n')
    # Freeze the protocol before any candidate output is observed.
    rows=[]
    variants=[('space-placeholder','<img src="   " data-original="{url}">'),
      ('blob-placeholder','<img src="blob:opaque" data-src="{url}">'),
      ('data-lazy-set','<img src="data:image/gif;base64,AA==" data-lazy-srcset="{url} 900w">'),
      ('ordinary','<img src="{url}">')]
    for i in range(24):
        kind,template=variants[i%len(variants)];token='PIC_'+secrets.token_hex(5)
        url=f'https://example.com/gallery/{token}.jpg'
        text=f'Frozen acceptance image {i}. 東京 数据 information for this illustrated document.'
        body=f'<html><body><main><h1>{token}</h1><p>{html.escape(text)}</p>{template.format(url=url)}</main></body></html>'
        p=DATA/f'image-{i}.html';p.write_text(body)
        rows.append({'id':f'holdout-image-{i}','path':str(p),'base_url':'https://example.com/gallery/index','only_main':False,'gold_images':[url],
                     'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'kind':kind})
    (DATA/'images.jsonl').write_text(''.join(json.dumps(r)+'\n' for r in rows))
    def fetch(row):
        start=time.monotonic();result=dict(row,status='not-fetched')
        try:
            robots_url=row['url']+'robots.txt';rp=urllib.robotparser.RobotFileParser()
            try:
                with urllib.request.urlopen(urllib.request.Request(robots_url,headers={'User-Agent':UA}),timeout=7) as r:
                    rp.parse(r.read(262144).decode(errors='replace').splitlines())
            except urllib.error.HTTPError as e:
                if e.code in (404,410):rp.allow_all=True
                else:result.update(status='robots-http-error',http=e.code);return result
            except Exception as e:result.update(status='robots-unavailable',error=repr(e));return result
            if not rp.can_fetch(UA,row['url']):result['status']='robots-disallowed';return result
            time.sleep(.5)
            with urllib.request.urlopen(urllib.request.Request(row['url'],headers={'User-Agent':UA,'Accept':'text/html'}),timeout=10) as r:
                body=r.read(2*1024*1024+1);result.update(final_url=r.url,http=r.status,content_type=r.headers.get('Content-Type',''))
            if len(body)>2*1024*1024:result['status']='too-large';return result
            if 'html' not in result['content_type']:result['status']='non-html';return result
            p=DATA/f"live-{row['rank']}.html";p.write_bytes(body)
            result.update(status='fetched',path=str(p),bytes=len(body),sha256=hashlib.sha256(body).hexdigest())
        except Exception as e:result.update(status='fetch-error',error=repr(e))
        finally:result['wall_s']=time.monotonic()-start
        return result
    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:live=list(pool.map(fetch,protocol['live_sample']))
    (OUT/('live-fetch'+LABEL+'.json')).write_text(json.dumps(live,indent=2)+'\n')
    print(json.dumps({'master_seed':seed,'images':len(rows),'live_statuses':{s:sum(r['status']==s for r in live) for s in sorted({r['status'] for r in live})}}))


if __name__=='__main__':main()

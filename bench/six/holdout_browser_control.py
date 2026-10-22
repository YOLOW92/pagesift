#!/usr/bin/env python3
"""Prove native browser execution independently of adaptive mode selection."""
import json,os,subprocess,sys,threading
from pathlib import Path
from http.server import ThreadingHTTPServer
from holdout_render import Origin
ROOT=Path(__file__).resolve().parents[2]


def main():
    out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
    families=json.loads((ROOT/'bench/six/results/round4/protocol-v2.json').read_text())['adaptive_families']
    origin=ThreadingHTTPServer(('127.0.0.1',0),Origin);origin.pages={};origin.requests=[]
    base=f'http://127.0.0.1:{origin.server_port}';urls=[];gold={}
    for f in families:
        slug=f['slug'];kind=f['kind'];static=[f'/{slug}/notes/{i}' for i in range(4)];dynamic=f'/{slug}/application'
        if kind=='different-segment':dynamic=f'/{slug}app/detail'
        if kind=='query-route':static=[f'/{slug}/view?mode=static&i={i}' for i in range(4)];dynamic=f'/{slug}/view?mode=dynamic'
        for path in static:origin.pages[path]={'kind':kind};urls.append(base+path)
        origin.pages[dynamic]={'kind':kind,'answer':f['answer'],'slug':slug};urls.append(base+dynamic);gold[base+dynamic]=f
    threading.Thread(target=origin.serve_forever,daemon=True).start()
    manifest=out/'urls.json';manifest.write_text(json.dumps(urls))
    env=dict(os.environ,CRAWLEE_STORAGE_DIR=str(out/'store'),CRAWLEE_PURGE_ON_START='true')
    try:
        with (out/'service.log').open('w') as log:
            subprocess.run(['node','bench/six/adapters/browser_control.mjs',str(manifest),str(out/'data.json')],env=env,stdout=log,stderr=log,timeout=180,check=True)
        items={r['url']:r for r in json.loads((out/'data.json').read_text())['items']}
        details=[{'kind':f['kind'],'answer_found':f['answer'] in items.get(u,{}).get('text','')} for u,f in gold.items()]
        result={'mode':'native PlaywrightCrawler with explicit networkidle wait','n':len(details),'found':sum(r['answer_found'] for r in details),'details':details,
            'scope':'browser execution control; not default adaptive behavior or a latency comparison'}
        (out/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
    finally:origin.shutdown();origin.server_close()


if __name__=='__main__':main()

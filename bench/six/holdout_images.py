#!/usr/bin/env python3
"""New frozen image cases: full HTTP APIs plus disclosed native components."""
import hashlib,json,socket,subprocess,sys,threading,time,urllib.request
from pathlib import Path
from http.server import ThreadingHTTPServer
from firecrawl_full import Origin,request

ROOT=Path(__file__).resolve().parents[2]


def main():
    manifest=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve();out.mkdir(parents=True,exist_ok=False)
    rows=list(map(json.loads,manifest.read_text().splitlines()))
    for r in rows:assert hashlib.sha256(Path(r['path']).read_bytes()).hexdigest()==r['sha256']
    origin=ThreadingHTTPServer(('127.0.0.1',0),Origin);origin.pages={r['id']:r for r in rows};origin.requests=[]
    threading.Thread(target=origin.serve_forever,daemon=True).start()
    summary={};active=None
    try:
        for name,binary in zip(('baseline','candidate'),sys.argv[3:5],strict=True):
            with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
            cfg=out/f'{name}.toml';cfg.write_text('[fetch]\nallow_private_network = true\n[policy]\ninterval_ms = 0\n[cache]\nenabled = false\ndir = '+json.dumps(str(out/name))+'\n')
            with (out/f'{name}.log').open('w') as log:
                active=subprocess.Popen([binary,'serve','--port',str(port),'--config',str(cfg)],stdout=log,stderr=log)
                api=f'http://127.0.0.1:{port}'
                for _ in range(100):
                    try:
                        with urllib.request.urlopen(api+'/healthz',timeout=.5):break
                    except OSError:
                        if active.poll() is not None:raise RuntimeError('API failed')
                        time.sleep(.1)
                else:raise RuntimeError('API readiness')
                records=[]
                for row in rows:
                    code,response=request(api+'/v2/scrape',{'url':f'http://127.0.0.1:{origin.server_port}/case/{row["id"]}',
                          'engine':'http','onlyMainContent':False,'formats':['markdown','images']})
                    data=response.get('data') or {}
                    records.append({'id':row['id'],'http':code,'response':response,'image_field_hit':any(u in data.get('images',[]) for u in row['gold_images']),
                        'markdown_hit':any(u in data.get('markdown','') for u in row['gold_images'])})
                (out/f'{name}.json').write_text(json.dumps(records,indent=2)+'\n')
                summary[name]={'n':len(rows),'api_errors':sum(r['http']!=200 for r in records),
                    'image_field_hits':sum(r['image_field_hit'] for r in records),'markdown_hits':sum(r['markdown_hit'] for r in records)}
                active.terminate();active.wait(timeout=15);active=None
        for tool in ('gallery-dl','scrapling'):
            path=out/(tool+'.jsonl')
            subprocess.run([str(ROOT/'bench/data/six/venv/bin/python'),'bench/six/adapters/parser.py',tool,str(manifest),str(path)],check=True,timeout=60)
            records=list(map(json.loads,path.read_text().splitlines()));by={r['id']:r for r in records}
            summary[tool]={'n':len(rows),'errors':sum(r['status']!='ok' for r in records),
                'image_field_hits':sum(any(u in by[r['id']].get('images',[]) for u in r['gold_images']) for r in rows),
                'scope':'native GenericExtractor' if tool=='gallery-dl' else 'raw img::attr(src) selector only; not smart image extraction'}
        for tool in ('essence','essence-main'):
            binary=ROOT/('bench/data/six/bin/offline-main' if tool.endswith('main') else 'bench/data/six/bin/offline-stable')
            path=out/(tool+'.jsonl');subprocess.run([str(binary),'essence',str(manifest),str(path)],check=True,timeout=60)
            by={r['id']:r for r in map(json.loads,path.read_text().splitlines())}
            summary[tool]={'n':len(rows),'errors':sum(r['status']!='ok' for r in by.values()),
                'markdown_hits':sum(any(u in by[r['id']].get('markdown','') for u in r['gold_images']) for r in rows),'scope':'unchanged offline Markdown component'}
        summary['candidate_passes']=summary['candidate']['image_field_hits']==len(rows) and summary['candidate']['api_errors']==0
        (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
    finally:
        if active is not None:active.kill();active.wait(timeout=10)
        origin.shutdown();origin.server_close()


if __name__=='__main__':main()

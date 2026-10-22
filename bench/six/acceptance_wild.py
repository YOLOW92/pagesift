#!/usr/bin/env python3
"""Original wild origin, fresh seed, private process lifecycle (no global pkill)."""
import hashlib,json,os,socket,subprocess,time,urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4/wild-final'


def main():
    OUT.mkdir(parents=True,exist_ok=False)
    seed=json.loads((ROOT/'bench/six/results/round4/protocol-v2.json').read_text())['master_seed']
    protocol={'seed':seed,'hosts':40,'pages_per_host':60,'global_concurrency':8,
      'gates':'zero robots violations; candidate coverage no more than 2 percentage points below baseline per scenario (time-based rate limits introduce scheduling variation)',
      'scope':'legacy crawling quality regression; no competitor throughput ranking'}
    (OUT/'protocol.json').write_text(json.dumps(protocol,indent=2)+'\n')
    get=urllib.request.build_opener(urllib.request.ProxyHandler({}));summary={};active=None
    try:
        for scenario,interval,per_host in [('fast',0,4),('polite',200,1)]:
            summary[scenario]={}
            for name,binary in [('baseline',ROOT/'target/release/examples/crawl_bench'),('candidate',ROOT/'bench/data/six/acceptance/source-v2/target/release/examples/crawl_bench')]:
                with socket.socket(socket.AF_INET6) as s:s.bind(('::1',0));port=s.getsockname()[1]
                with (OUT/f'{scenario}-{name}-origin.log').open('w') as log:
                    active=subprocess.Popen([str(ROOT/'bench/crawl/target/release/wild'),str(ROOT/'bench/data/six/aeb'),str(port),'40','60','20',str(seed)],stdout=log,stderr=log)
                    def fetch(path):
                        req=urllib.request.Request(f'http://[::1]:{port}'+path,headers={'Host':'s0.localhost'})
                        with get.open(req,timeout=5) as r:return json.load(r)
                    for _ in range(100):
                        try:fetch('/__served');break
                        except OSError:
                            if active.poll() is not None:raise RuntimeError('wild origin failed')
                            time.sleep(.1)
                    else:raise RuntimeError('wild origin readiness')
                    env=dict(os.environ,GLOBAL='8',ENGINE='http')
                    with (OUT/f'{scenario}-{name}.log').open('w') as log2:
                        subprocess.run([str(binary),str(port),'40','60',str(interval),str(per_host)],env=env,stdout=log2,stderr=subprocess.STDOUT,check=True,timeout=240)
                    report=fetch('/__report');(OUT/f'{scenario}-{name}.json').write_text(json.dumps(report,indent=2)+'\n')
                    unique=sum(s['unique_content'] for s in report)
                    robots=sum(s['kinds'].get('robots',0) for s in report)
                    summary[scenario][name]={'unique_content':unique,'coverage':unique/2400,'robots_violations':robots,'requests':sum(s['requests'] for s in report)}
                    active.terminate();active.wait(timeout=10);active=None
                    (OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
        summary['passes']=all(v['candidate']['robots_violations']==0 and v['candidate']['coverage']>=v['baseline']['coverage']-.02 for v in summary.values())
        summary['origin_sha256']=hashlib.sha256((ROOT/'bench/crawl/target/release/wild').read_bytes()).hexdigest()
        (OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
    finally:
        if active is not None:active.kill();active.wait(timeout=10)


if __name__=='__main__':main()

#!/usr/bin/env python3
"""Bounded, disposable Firecrawl stack. No production services, ports or volumes touched."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'
PROJECT = 'pagesift-six-firecrawl'


def command(args, **kw):
    return subprocess.run(args, check=True, text=True, **kw)


def prepare():
    pins = json.loads((ROOT / 'bench/six/locks/firecrawl-images.json').read_text())
    runtime_path = ROOT / 'bench/six/locks/firecrawl-runtime.json'
    previous = json.loads(runtime_path.read_text()) if runtime_path.exists() else {}
    images = {name: previous.get(name, {}).get('image', image) for name, image in
              [('api', pins['firecrawl']['image']), ('playwright', pins['playwright-service']['image']),
               ('postgres', pins['nuq-postgres']['image']), ('redis', 'redis:7-alpine'), ('rabbitmq', 'rabbitmq:3-management')]}
    for image in images.values():
        if shutil.disk_usage(ROOT).free < 12 * 2**30: raise RuntimeError('disk reserve before pull')
        command(['docker', 'pull', image])
    runtime = {}
    for name, image in images.items():
        d = json.loads(subprocess.check_output(['docker','image','inspect',image],text=True))[0]
        runtime[name] = {'image': d['RepoDigests'][0], 'id': d['Id'], 'size': d['Size'],
                         'pg_major': next((v for v in d['Config'].get('Env',[]) if v.startswith('PG_MAJOR=')), None)}
    if runtime['postgres']['pg_major'] != 'PG_MAJOR=17':
        raise RuntimeError('locked source expects PostgreSQL 17')
    (ROOT / 'bench/six/locks/firecrawl-runtime.json').write_text(json.dumps(runtime,indent=2)+'\n')
    # The auxiliary images are independently versioned. Run the SQL schema from
    # the API's own locked source, not an accidental newer queue schema.
    common = {'cpuset': '0-7', 'pids_limit': 512, 'restart': 'no',
              'logging': {'driver':'json-file','options':{'max-size':'5m','max-file':'1'}}}
    def service(name, mb, cpus, **extra):
        return dict(common, image=runtime[name]['image'], mem_limit=f'{mb}m', memswap_limit=f'{mb}m', cpus=cpus, **extra)
    pg_sql = str(DATA/'sources/firecrawl/apps/nuq-postgres/nuq.sql')
    environment = {'ENV':'local','HOST':'0.0.0.0','PORT':'3002','USE_DB_AUTHENTICATION':'false',
                   'REDIS_URL':'redis://redis:6379','REDIS_RATE_LIMIT_URL':'redis://redis:6379',
                   'POSTGRES_HOST':'postgres','POSTGRES_USER':'postgres','POSTGRES_PASSWORD':'comparison-only',
                   'POSTGRES_DB':'postgres','POSTGRES_PORT':'5432','NUQ_BACKEND':'pg',
                   'NUQ_RABBITMQ_URL':'amqp://rabbitmq:5672',
                   'PLAYWRIGHT_MICROSERVICE_URL':'http://playwright:3000/scrape',
                   'NUM_WORKERS_PER_QUEUE':'1','NUQ_WORKER_COUNT':'1','CRAWL_CONCURRENT_REQUESTS':'2',
                   'MAX_CONCURRENT_JOBS':'2','BROWSER_POOL_SIZE':'1','ALLOW_LOCAL_WEBHOOKS':'true',
                   'HARNESS_STARTUP_TIMEOUT_MS':'120000','NODE_OPTIONS':'--max-old-space-size=768',
                   'LOGGING_LEVEL':'warn'}
    services = {
        'api': service('api',4096,4, environment=environment, extra_hosts=['host.docker.internal:host-gateway','replay.pagesift.example.com:host-gateway'],
                       ports=['127.0.0.1:13992:3002'],
                       depends_on={'redis':{'condition':'service_healthy'},'postgres':{'condition':'service_healthy'},
                                   'rabbitmq':{'condition':'service_healthy'},'playwright':{'condition':'service_started'}},
                       command=['node','dist/src/harness.js','--start-docker']),
        'playwright':service('playwright',1536,2,environment={'PORT':'3000','MAX_CONCURRENT_PAGES':'1','ALLOW_LOCAL_WEBHOOKS':'true'},
                             extra_hosts=['host.docker.internal:host-gateway','replay.pagesift.example.com:host-gateway']),
        'postgres':service('postgres',512,.5,environment={'POSTGRES_USER':'postgres','POSTGRES_PASSWORD':'comparison-only','POSTGRES_DB':'postgres'},
                          volumes=[pg_sql+':/docker-entrypoint-initdb.d/010-nuq.sql:ro'],
                          healthcheck={'test':['CMD','pg_isready','-h','127.0.0.1','-U','postgres','-d','postgres'],
                                       'interval':'2s','timeout':'2s','retries':30,'start_period':'5s'}),
        'redis':service('redis',128,.2,command=['redis-server','--maxmemory','96mb','--maxmemory-policy','noeviction'],
                        healthcheck={'test':['CMD','redis-cli','ping'],'interval':'2s','timeout':'2s','retries':20}),
        'rabbitmq':service('rabbitmq',512,.5,environment={'RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS':'+S 2:2'},
                           healthcheck={'test':['CMD','rabbitmq-diagnostics','-q','check_running'],
                                        'interval':'3s','timeout':'5s','retries':20,'start_period':'10s'}),
    }
    config={'name':PROJECT,'services':services}
    path=DATA/'firecrawl-compose.json';path.write_text(json.dumps(config,indent=2)+'\n')
    print(json.dumps({'runtime_memory_limit_mb':6784,'runtime_cpu_limit':7.2,'config':str(path)}))


def main():
    p=argparse.ArgumentParser();p.add_argument('mode',choices=['prepare','up','down','logs']);a=p.parse_args()
    if a.mode=='prepare': prepare();return
    args=[str(DATA/'bin/docker-compose'),'-p',PROJECT,'-f',str(DATA/'firecrawl-compose.json')]
    if a.mode=='up': command(args+['up','-d','--pull','never'])
    elif a.mode=='down': command(args+['down','--volumes','--remove-orphans'])
    else: command(args+['logs','--no-color','--tail','150'])


if __name__=='__main__': main()

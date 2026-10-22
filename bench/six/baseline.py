#!/usr/bin/env python3
"""Serial offline baseline. Run through run.py; it bounds all child processes together."""
import json
import hashlib
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'


def validate(manifest, output):
    with manifest.open() as f:
        wanted = [r['id'] for r in map(json.loads, f)]
    with output.open() as f:
        got = [r['id'] for r in map(json.loads, f)]
    if len(wanted) != len(set(wanted)):
        raise RuntimeError(f'duplicate input sample IDs: {manifest}')
    if len(got) != len(set(got)) or set(got) != set(wanted):
        raise RuntimeError(f'incomplete/duplicate sample IDs: {output}')


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    offline = str(DATA / 'bin/offline-stable')
    py = str(DATA / 'venv/bin/python')
    tasks = []
    for name in ['pagesift', 'essence']:
        for dataset in ['fixtures', 'aeb']:
            manifest = DATA / f'{dataset}.jsonl'
            output = out / f'{dataset}-{name}.jsonl'
            tasks.append((name, manifest, output, [offline, name, str(manifest), str(output)]))
    for name in ['scrapling', 'gallery-dl']:
        manifest = DATA / 'fixtures.jsonl'
        output = out / f'fixtures-{name}.jsonl'
        tasks.append((name, manifest, output,
                      [py, 'bench/six/adapters/parser.py', name, str(manifest), str(output)]))
    manifest = DATA / 'fixtures.jsonl'
    output = out / 'fixtures-firecrawl-go-component.jsonl'
    tasks.append(('firecrawl-go-component', manifest, output,
                  [str(DATA / 'bin/firecrawl-converter'), str(manifest), str(output)]))
    records = []
    for name, manifest, output, command in tasks:
        with manifest.open() as inputs:
            for row in map(json.loads, inputs):
                if row.get('sha256') != hashlib.sha256(Path(row['path']).read_bytes()).hexdigest():
                    raise RuntimeError(f'input changed: {row["id"]}')
        t = time.monotonic()
        with output.with_suffix('.stderr.log').open('w') as log:
            proc = subprocess.run(command, cwd=ROOT, stderr=log, timeout=300)
        record = {'tool': name, 'dataset': manifest.name, 'command': command,
                  'wall_s': time.monotonic() - t, 'exit': proc.returncode}
        records.append(record)
        (out / 'tasks.json').write_text(json.dumps(records, indent=2) + '\n')
        if proc.returncode:
            raise RuntimeError(f'{name} infrastructure/crash; inspect {log.name}')
        validate(manifest, output)
        record['output_sha256'] = hashlib.sha256(output.read_bytes()).hexdigest()
        record['ids_complete'] = True
        (out / 'tasks.json').write_text(json.dumps(records, indent=2) + '\n')
        print(json.dumps(record), flush=True)


if __name__ == '__main__':
    main()

#!/usr/bin/env python3
"""Build thin, separate comparison binaries from locked, unmodified upstream modules."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'


def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser(); p.add_argument('--essence', choices=['stable', 'main'], default='stable')
    a = p.parse_args()
    source = DATA / 'sources' / ('essence' if a.essence == 'stable' else 'essence-main')
    env = dict(os.environ, CARGO_TARGET_DIR=str(DATA / 'rust-target'),
               ESSENCE_SOURCE=str(source / 'backend/src/format'))
    subprocess.run(['cargo', 'build', '--release', '--locked', '--manifest-path',
                    'bench/six/adapters/offline/Cargo.toml'], cwd=ROOT, env=env, check=True)
    binary = DATA / 'bin' / ('offline-' + a.essence)
    shutil.copy2(DATA / 'rust-target/release/pagesift-six-offline', binary)
    converter_source = DATA / 'sources/firecrawl/apps/go-html-to-md-service'
    work = DATA / 'firecrawl-converter'; work.mkdir(exist_ok=True)
    for filename in ['converter.go', 'go.mod', 'go.sum']:
        shutil.copy2(converter_source / filename, work / filename)
    shutil.copy2(ROOT / 'bench/six/adapters/converter_cli.go', work / 'main.go')
    env.update(GOMODCACHE=str(DATA / 'go-modules'), GOCACHE=str(DATA / 'go-cache'), GOMAXPROCS='8')
    subprocess.run([str(DATA / 'go/bin/go'), '-C', str(work), 'build', '-mod=readonly', '-p', '8',
                    '-o', str(DATA / 'bin/firecrawl-converter'), '.'], env=env, check=True)
    record = {'essence': json.loads((source / '.six-source.json').read_text()),
              'modules': {str(f.relative_to(ROOT)): sha(f) for f in [source / 'backend/src/format/markdown.rs',
                           source / 'backend/src/format/image_processing.rs', converter_source / 'converter.go']},
              'binaries': {str(f.relative_to(ROOT)): sha(f) for f in [binary, DATA / 'bin/firecrawl-converter']}}
    (DATA / ('build-' + a.essence + '.json')).write_text(json.dumps(record, indent=2) + '\n')


if __name__ == '__main__': main()

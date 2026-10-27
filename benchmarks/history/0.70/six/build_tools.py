#!/usr/bin/env python3
"""Build thin, separate comparison binaries from locked, unmodified upstream modules."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[4]
DATA = ROOT / 'benchmarks/data/six'


def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser(); p.add_argument('--essence', choices=['stable', 'main'], default='stable')
    a = p.parse_args()
    source = DATA / 'sources' / ('essence' if a.essence == 'stable' else 'essence-main')
    env = dict(os.environ, CARGO_TARGET_DIR=str(DATA / 'rust-target'),
               ESSENCE_SOURCE=str(source / 'backend/src/format'))
    subprocess.run(['cargo', 'build', '--release', '--locked', '--manifest-path',
                    'benchmarks/history/0.70/six/adapters/offline/Cargo.toml'], cwd=ROOT, env=env, check=True)
    binary = DATA / 'bin' / ('offline-' + a.essence)
    shutil.copy2(DATA / 'rust-target/release/pagesift-six-offline', binary)
    record = {'essence': json.loads((source / '.six-source.json').read_text()),
              'modules': {str(f.relative_to(ROOT)): sha(f) for f in [source / 'backend/src/format/markdown.rs', source / 'backend/src/format/image_processing.rs']},
              'binaries': {str(f.relative_to(ROOT)): sha(f) for f in [binary]}}
    (DATA / ('build-' + a.essence + '.json')).write_text(json.dumps(record, indent=2) + '\n')


if __name__ == '__main__': main()

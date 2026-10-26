#!/usr/bin/env python3
"""Fetch locked source archives without Git history or downloaded browsers."""
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'


def fetch(url, destination, limit=100 << 20):
    h = hashlib.sha256()
    total = 0
    with urllib.request.urlopen(url, timeout=60) as source, destination.open('wb') as target:
        while block := source.read(1 << 20):
            total += len(block)
            if total > limit:
                raise RuntimeError('download budget exceeded')
            h.update(block)
            target.write(block)
    return h.hexdigest()


def main():
    lock = json.loads((ROOT / 'bench/six/tools.lock.json').read_text())
    DATA.mkdir(parents=True, exist_ok=True)
    inventory = {}
    for name, tool in lock['tools'].items():
        if shutil.disk_usage(DATA).free < 12 * 2**30:
            raise RuntimeError('disk reserve reached')
        destination = DATA / 'sources' / name
        if destination.exists():
            marker = json.loads((destination / '.six-source.json').read_text())
            if marker['sha'] != tool['sha']:
                raise RuntimeError(f'{name}: source version differs; use a new destination')
            inventory[name] = marker
            continue
        archive = DATA / f'{name}.tar.gz.part'
        sha = fetch(tool['archive_url'], archive)
        stage = DATA / 'sources' / (name + '.partial')
        stage.mkdir(parents=True, exist_ok=False)
        with tarfile.open(archive) as tar:
            members = tar.getmembers()
            if sum(m.size for m in members) > 300 << 20:
                raise RuntimeError('expanded source budget exceeded')
            # Drop archive root; never follow upstream links or special files.
            for m in members:
                parts = Path(m.name).parts[1:]
                if not parts or '..' in parts or m.issym() or m.islnk() or not (m.isfile() or m.isdir()):
                    continue
                m.name = str(Path(*parts))
                tar.extract(m, stage, filter='data')
        marker = {'sha': tool['sha'], 'tag': tool['tag'], 'archive_url': tool['archive_url'],
                  'archive_sha256': sha}
        (stage / '.six-source.json').write_text(json.dumps(marker, indent=2) + '\n')
        stage.rename(destination)
        archive.unlink()
        inventory[name] = marker
        print(name, tool['tag'], sha, flush=True)
    (DATA / 'source-inventory.json').write_text(json.dumps(inventory, indent=2) + '\n')


if __name__ == '__main__':
    main()

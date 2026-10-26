#!/usr/bin/env python3
"""Install isolated small dependencies and verified release binaries, never browsers."""
import hashlib
import shutil
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[4]
DATA = ROOT / 'benchmarks/data/six'


def download(url, path):
    if not path.exists():
        with urllib.request.urlopen(url, timeout=60) as src, path.open('wb') as dst:
            size = 0
            while block := src.read(1 << 20):
                size += len(block)
                if size > 100 << 20:
                    raise RuntimeError('download budget')
                dst.write(block)
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    binaries = DATA / 'bin'
    binaries.mkdir(exist_ok=True)
    record = {}
    downloads = [
        ('katana', 'https://github.com/projectdiscovery/katana/releases/download/v1.8.0/',
         'katana_1.8.0_linux_amd64.zip', 'katana-1.8.0-checksums.txt'),
        ('essence', 'https://github.com/ruchit-p/essence/releases/download/v0.3.0/',
         'essence-v0.3.0-x86_64-unknown-linux-gnu.tar.gz', 'essence-v0.3.0-x86_64-unknown-linux-gnu.tar.gz.sha256')]
    for name, base, filename, sums in downloads:
        archive = DATA / filename
        actual = download(base + filename, archive)
        checksum_file = DATA / sums
        download(base + sums, checksum_file)
        matching = [line.split()[0] for line in checksum_file.read_text().splitlines() if filename in line]
        if not matching or matching[0].lower() != actual:
            raise RuntimeError(f'{name}: checksum mismatch')
        if filename.endswith('.zip'):
            with zipfile.ZipFile(archive) as z:
                member = next(n for n in z.namelist() if Path(n).name == name)
                (binaries / name).write_bytes(z.read(member))
        else:
            with tarfile.open(archive) as tar:
                member = next(m for m in tar.getmembers() if Path(m.name).name == name and m.isfile())
                (binaries / name).write_bytes(tar.extractfile(member).read())
        (binaries / name).chmod(0o755)
        record[name] = {'archive_sha256': actual, 'url': base + filename,
                        'binary_sha256': hashlib.sha256((binaries / name).read_bytes()).hexdigest()}
        archive.unlink()
        print(name, 'verified', flush=True)
    (DATA / 'binaries.json').write_text(json.dumps(record, indent=2) + '\n')
    venv = DATA / 'venv'
    if not (venv / 'bin/python').exists():
        subprocess.run([sys.executable, '-m', 'venv', str(venv)], check=True)
    py = str(venv / 'bin/python')
    subprocess.run([py, '-m', 'pip', 'install', '--no-cache-dir', str(DATA / 'sources/scrapling'),
                    str(DATA / 'sources/gallery-dl'), 'pytest', 'pytest-asyncio', 'requests', 'markdownify'], check=True)
    with (DATA / 'python-freeze.txt').open('w') as f:
        subprocess.run([py, '-m', 'pip', 'freeze', '--all'], stdout=f, check=True)
    node = DATA / 'node'
    node.mkdir(exist_ok=True)
    for name in ['package.json', 'package-lock.json']:
        shutil.copy2(ROOT / 'benchmarks/history/0.70/six/locks' / name, node / name)
    subprocess.run(['npm', 'ci', '--no-audit', '--no-fund'], cwd=node, check=True)


if __name__ == '__main__':
    main()

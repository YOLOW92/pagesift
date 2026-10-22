#!/usr/bin/env python3
"""Publish workspace crates in dependency order, skipping verified existing versions."""
import argparse
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
import json
import re
import subprocess
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from check_release import check


def order(packages):
    publishable = {p['name']: p for p in packages if p.get('publish') != []}
    seen, active, result = set(), set(), []

    def visit(name):
        if name in seen:
            return
        if name in active:
            raise ValueError(f'cyclic dependency: {name}')
        active.add(name)
        for dep in publishable[name]['dependencies']:
            if dep['name'] in publishable and dep.get('kind') in (None, 'build'):
                visit(dep['name'])
        active.remove(name)
        seen.add(name)
        result.append(publishable[name])

    for name in sorted(publishable):
        visit(name)
    return result


def exists(name, version):
    url = f'https://crates.io/api/v1/crates/{name}/{version}'
    request = Request(url, headers={'User-Agent': 'pagesift-release/0.20.0 (github.com/YOLOW92/pagesift)'})
    for attempt in range(6):
        try:
            with urlopen(request, timeout=30) as response:
                return json.load(response)['version']['num'] == version
        except HTTPError as error:
            if error.code == 404:
                return False
            if error.code not in (429, 500, 502, 503, 504) or attempt == 5:
                raise
        time.sleep(10 * (attempt+1))
    raise RuntimeError('unreachable')


def retry_delay(output):
    match = re.search(r'after ([A-Z][a-z]+, \d+ [A-Z][a-z]+ \d+ [\d:]+ GMT)', output)
    if match:
        return max(30, int((parsedate_to_datetime(match[1])-datetime.now(timezone.utc)).total_seconds())+15)
    return 615


def publish(package):
    name, version = package['name'], package['version']
    if exists(name, version):
        print(f'{name} {version}: already published', flush=True)
        return
    for attempt in range(20):
        result = subprocess.run(['cargo', 'publish', '-p', name, '--locked'], text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(result.stdout, flush=True)
        if result.returncode == 0 or 'already exists' in result.stdout:
            # Registry API visibility can lag the upload; do not silently accept a failed upload.
            for _ in range(12):
                if exists(name, version):
                    return
                time.sleep(10)
            raise RuntimeError(f'{name} uploaded but not visible in registry API')
        if '429' not in result.stdout:
            raise RuntimeError(f'{name}: cargo publish failed')
        wait = retry_delay(result.stdout)
        print(f'{name}: rate limited, wait {wait}s (attempt {attempt+1}/20)', flush=True)
        time.sleep(wait)
    raise RuntimeError(f'{name}: retry budget exhausted')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    packages = order(check()['packages'])
    print('Publish order: ' + ', '.join(p['name'] for p in packages), flush=True)
    if not args.dry_run:
        for package in packages:
            publish(package)

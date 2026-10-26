#!/usr/bin/env python3
"""Publish workspace crates in dependency order, skipping verified existing versions."""
import argparse
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
import json
import os
import re
import subprocess
import time
from urllib.error import HTTPError
from urllib.request import Request, urlopen
from check_release import check


USER_AGENT = 'pagesift-release/0.20.0 (github.com/YOLOW92/pagesift)'
TOKENS_URL = 'https://crates.io/api/v1/trusted_publishing/tokens'
TOKEN_MAX_AGE = 20 * 60   # crates.io 的 Trusted Publishing 临时令牌 30 分钟过期，提前换新


class Token:
    """CARGO_REGISTRY_TOKEN 的来源。

    工作流里先由 rust-lang/crates-io-auth-action 用 OIDC 换到临时令牌（环境变量 CARGO_REGISTRY_TOKEN）；
    令牌只有 30 分钟有效，而依次发 15 个 crate 加限流等待可能远超这个时间，
    所以在 GitHub Actions 里（有 ACTIONS_ID_TOKEN_REQUEST_URL）本脚本会自己用同样的 OIDC 流程续换。
    不在 Actions 里时（本机手动补发）就一直用环境变量里的令牌，不续换。
    """

    def __init__(self):
        self.value = os.environ.get('CARGO_REGISTRY_TOKEN')
        self.born = time.monotonic()
        self.minted = []   # 本脚本自己换的令牌，退出时吊销（action 只吊销它自己换的那个）

    @staticmethod
    def can_mint():
        return bool(os.environ.get('ACTIONS_ID_TOKEN_REQUEST_URL') and os.environ.get('ACTIONS_ID_TOKEN_REQUEST_TOKEN'))

    def mint(self):
        request = Request(os.environ['ACTIONS_ID_TOKEN_REQUEST_URL'] + '&audience=crates.io',
                          headers={'Authorization': 'bearer ' + os.environ['ACTIONS_ID_TOKEN_REQUEST_TOKEN'],
                                   'User-Agent': USER_AGENT})
        with urlopen(request, timeout=30) as response:
            jwt = json.load(response)['value']
        request = Request(TOKENS_URL, data=json.dumps({'jwt': jwt}).encode(),
                          headers={'Content-Type': 'application/json', 'User-Agent': USER_AGENT})
        with urlopen(request, timeout=30) as response:
            token = json.load(response)['token']
        print(f'::add-mask::{token}', flush=True)
        self.minted.append(token)
        return token

    def get(self):
        if self.can_mint() and (not self.value or time.monotonic() - self.born > TOKEN_MAX_AGE):
            try:
                self.value, self.born = self.mint(), time.monotonic()
            except Exception as error:   # 换不到就继续用手里的；真过期了 cargo publish 会明确报错
                if not self.value:
                    raise
                print(f'warning: renewing crates.io token failed: {error}', flush=True)
        if not self.value:
            raise RuntimeError('no crates.io token: set CARGO_REGISTRY_TOKEN or run with id-token: write')
        return self.value

    def revoke(self):
        for token in self.minted:
            try:
                urlopen(Request(TOKENS_URL, method='DELETE',
                                headers={'Authorization': 'Bearer ' + token, 'User-Agent': USER_AGENT}), timeout=30)
            except Exception as error:
                print(f'warning: revoking token failed: {error}', flush=True)


TOKEN = Token()


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
    request = Request(url, headers={'User-Agent': USER_AGENT})
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
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                env={**os.environ, 'CARGO_REGISTRY_TOKEN': TOKEN.get()})
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
        try:
            for package in packages:
                publish(package)
        finally:
            TOKEN.revoke()

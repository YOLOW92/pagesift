#!/usr/bin/env python3
"""Fail before publishing if workspace, native packages or a release tag disagree."""
import argparse
import json
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def check(tag=None):
    workspace = tomllib.loads((ROOT / 'Cargo.toml').read_text())
    version = workspace['workspace']['package']['version']
    if tag and tag != 'v' + version:
        raise ValueError(f'tag {tag} disagrees with workspace {version}')
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--no-deps', '--locked', '--format-version', '1'], cwd=ROOT))
    names = {p['name'] for p in metadata['packages']}
    for package in metadata['packages']:
        assert package['version'] == version, package['name']
        for dep in package['dependencies']:
            if dep['name'] in names and dep.get('path') and dep.get('kind') != 'dev':
                assert dep['req'] in ('^'+version, '='+version), (package['name'], dep)
    node = ROOT / 'crates/pagesift-node'
    platforms = sorted((node/'npm').glob('*/package.json'))
    node_config = json.loads((node/'package.json').read_text())
    expected = {'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu',
                'x86_64-unknown-linux-musl', 'x86_64-pc-windows-msvc',
                'x86_64-apple-darwin', 'aarch64-apple-darwin'}
    assert set(node_config['napi']['targets']) == expected, 'Node target matrix incomplete'
    assert len(platforms) == len(expected), 'Node platform packages incomplete'
    for path in [node/'package.json', *platforms]:
        assert json.loads(path.read_text())['version'] == version, path
    lock = json.loads((node/'package-lock.json').read_text())
    assert lock['version'] == lock['packages']['']['version'] == version
    pyproject = tomllib.loads((ROOT/'crates/pagesift-py/pyproject.toml').read_text())
    assert 'version' in pyproject['project']['dynamic']
    # Python 包目录里的 LICENSE、NOTICE 是根目录文件的副本（随 wheel 和 sdist 走），不能漂移。
    # 不能放在 crate 目录：maturin 会把它们写成 sdist 根目录的 License-File，而工作区 crate 的 sdist
    # 以工作区为根，PyPI 找不到文件就拒收（0.80.1 发布时踩过）
    pkg = ROOT/'crates/pagesift-py/python/pagesift'
    for name in ('LICENSE', 'NOTICE'):
        assert (pkg/name).read_bytes() == (ROOT/name).read_bytes(), f'python/pagesift/{name} differs from {name}'
        assert not (ROOT/'crates/pagesift-py'/name).exists(), f'crates/pagesift-py/{name} must not exist (breaks the sdist)'
    assert (ROOT/f'docs/releases/{version}.md').is_file(), 'release notes missing'
    print(f'Release identity OK: {version}, {len(names)} Rust packages, {len(platforms)} npm platforms, dynamic Python version')
    return metadata


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag')
    check(parser.parse_args().tag)

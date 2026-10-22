#!/usr/bin/env python3
"""Use the repository release guide as GitHub release notes, resolving relative links."""
from pathlib import Path
import posixpath
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]
version = tomllib.loads((ROOT/'Cargo.toml').read_text())['workspace']['package']['version']
source = Path(f'docs/releases/{version}.md')


def resolve(match):
    prefix, target = match.groups()
    if '://' in target or target.startswith('#'):
        return match[0]
    path = posixpath.normpath((source.parent / target).as_posix())
    kind = 'raw' if path.endswith('.svg') else 'blob'
    return f'{prefix}(https://github.com/YOLOW92/pagesift/{kind}/v{version}/{path})'


print(re.sub(r'(!?\[[^\]]*\])\(([^)]+)\)', resolve, (ROOT/source).read_text()))

#!/usr/bin/env python3
"""Transparent fixture diagnostic; not a real-world accuracy or full-product ranking."""
import json
from pathlib import Path
import re
import sys
from urllib.parse import urljoin

ROOT = Path(__file__).resolve().parents[2]


def load(path):
    with Path(path).open() as f:
        rows = list(map(json.loads, f))
    if len({r['id'] for r in rows}) != len(rows):
        raise ValueError(f'duplicate IDs: {path}')
    return {r['id']: r for r in rows}


def main():
    directory = Path(sys.argv[1])
    fixtures = load(ROOT / 'bench/data/six/fixtures.jsonl')
    results = {}
    for path in sorted(directory.glob('fixtures-*.jsonl')):
        records = load(path)
        if set(records) != set(fixtures):
            raise ValueError(f'incomplete IDs: {path}')
        detail = []
        for key, gold in fixtures.items():
            row = records[key]
            images = [urljoin(gold['base_url'], s) for s in row.get('images', [])]
            markdown_images = [urljoin(gold['base_url'], s) for s in
                               re.findall(r'!\[[^\]]*\]\(([^\s)]+)', row.get('markdown', ''))]
            detail.append({'id': key, 'status': row['status'], 'gold': gold['gold_images'],
                           'images': images, 'markdown_images': markdown_images,
                           'image_field_hit': any(s in images for s in gold['gold_images']),
                           'markdown_hit': any(s in markdown_images for s in gold['gold_images'])})
        results[path.stem] = {'n': len(detail), 'errors': sum(r['status'] != 'ok' for r in detail),
                              'image_field_hits': sum(r['image_field_hit'] for r in detail),
                              'markdown_hits': sum(r['markdown_hit'] for r in detail), 'detail': detail}
    (directory / 'fixture-scores.json').write_text(json.dumps(results, indent=2) + '\n')
    for tool, row in results.items():
        print(tool, {k: v for k, v in row.items() if k != 'detail'})


if __name__ == '__main__':
    main()

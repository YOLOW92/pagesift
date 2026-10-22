#!/usr/bin/env python3
"""Small, fully disclosed change/negative cases; baseline for a missing adaptive feature."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from scrapling import Selector

ROOT = Path(__file__).resolve().parents[2]


def main():
    out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=True)
    old = '<section><article id="target" class="product"><h2>Widget 日本語</h2><p>Price 42</p></article></section>'
    cases = [
        ('unchanged', old, 'Widget 日本語 Price 42'),
        ('id-to-data-id', old.replace('id="target"', 'data-id="target"'), 'Widget 日本語 Price 42'),
        ('wrapper', '<main><div>' + old.replace('id="target"', 'data-id="target"') + '</div></main>', 'Widget 日本語 Price 42'),
        ('class-change', old.replace('class="product"', 'class="item"'), 'Widget 日本語 Price 42'),
        ('removed', '<section><p>No products remain.</p></section>', None),
        ('lookalike', '<section><article id="other" class="product"><h2>Other 日本語</h2><p>Price 42</p></article></section>', None),
    ]
    path = out / 'original.html'; path.write_text(old)
    manifest = []
    rows = []
    with tempfile.TemporaryDirectory(prefix='scrapling-relocate-') as tmp:
        options = {'url': 'https://example.com/products/', 'adaptive': True,
                   'storage_args': {'storage_file': str(Path(tmp) / 'elements.db')}}
        before = Selector(old, **options)
        before.css('#target', auto_save=True)
        for key, html, gold in cases:
            page_path = out / (key + '.html'); page_path.write_text(html)
            manifest.append({'id': key, 'path': str(page_path), 'selector': '#target'})
            page = Selector(html, **options)
            started = time.perf_counter()
            exact = page.css('#target')
            relocated = exact if exact else page.css('#target', adaptive=True)
            values = [n.get_all_text(separator=' ', strip=True) for n in relocated]
            rows.append({'id': key, 'gold': gold, 'scrapling_values': values,
                         'scrapling_correct': [''.join(v.split()) for v in values] == ([''.join(gold.split())] if gold else []),
                         'ms': (time.perf_counter() - started) * 1000})
    manifest_path = out / 'manifest.jsonl'
    manifest_path.write_text(''.join(json.dumps(r) + '\n' for r in manifest))
    output = out / 'pagesift.jsonl'
    subprocess.run([str(ROOT / 'bench/data/six/baseline-bin/offline'), 'pagesift-select', str(manifest_path), str(output)], check=True)
    exact_rows = {r['id']: r for r in map(json.loads, output.open())}
    for row in rows:
        values = [' '.join(v.split()) for v in exact_rows[row['id']].get('values', [])]
        row['pagesift_exact_values'] = values
        row['pagesift_correct'] = [''.join(v.split()) for v in values] == ([''.join(row['gold'].split())] if row['gold'] else [])
    result = {'kind': 'synthetic capability diagnostic, not a population accuracy estimate',
              'scorer_version': 2, 'normalization': 'ignore whitespace consistently on both sides; measure relocation, not text separators',
              'scrapling': 'default 40% relocation threshold',
              'pagesift': 'existing exact selector; adaptive relocation is not implemented', 'cases': rows}
    (out / 'summary.json').write_text(json.dumps(result, indent=2, ensure_ascii=False) + '\n')
    print(json.dumps(result, ensure_ascii=False))


if __name__ == '__main__':
    main()

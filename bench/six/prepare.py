#!/usr/bin/env python3
"""Build transparent small gold fixtures and an unchanged AEB baseline manifest."""
import gzip
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'bench/data/six'


def main():
    fixtures = DATA / 'fixtures'
    fixtures.mkdir(exist_ok=True)
    images = [
        ('ordinary', '<img src="/photo.jpg" alt="Photo">', '/photo.jpg'),
        ('relative', '<img src="photo.jpg" alt="Photo">', '/article/photo.jpg'),
        ('data-src', '<img data-src="/photo.jpg" alt="Photo">', '/photo.jpg'),
        ('data-placeholder', '<img src="data:image/gif;base64,AAAA" data-src="/photo.jpg" alt="Photo">', '/photo.jpg'),
        ('blank-src', '<img src="  " data-src="/photo.jpg" alt="Photo">', '/photo.jpg'),
        ('lazy-src', '<img data-lazy-src="/photo.jpg" alt="Photo">', '/photo.jpg'),
        ('original', '<img data-original="/photo.jpg" alt="Photo">', '/photo.jpg'),
        ('invalid-src', '<img src="javascript:void(0)" data-src="/photo.jpg" alt="Photo">', '/photo.jpg'),
        ('srcset', '<img srcset="/small.jpg 400w, /photo.jpg 1200w" alt="Photo">', '/photo.jpg'),
        ('lazy-srcset', '<img data-srcset="/small.jpg 400w, /photo.jpg 1200w" alt="Photo">', '/photo.jpg'),
        ('picture', '<picture><source srcset="/small.jpg 400w, /photo.jpg 1200w"><img alt="Photo"></picture>', '/photo.jpg'),
        ('placeholder-srcset', '<img src="data:image/gif;base64,AAAA" srcset="/small.jpg 400w, /photo.jpg 1200w">', '/photo.jpg'),
    ]
    rows = []
    for name, image, target in images:
        html = f'<!doctype html><html><body><article><h1>Image example</h1><p>An illustration for the article.</p>{image}</article></body></html>'
        path = fixtures / (name + '.html')
        path.write_text(html)
        rows.append({'id': 'image-' + name, 'path': str(path), 'base_url': 'https://example.com/article/page',
                     'only_main': False, 'selector': 'h1', 'gold_values': ['Image example'],
                     'gold_images': ['https://example.com' + target], 'category': 'synthetic-image-discovery',
                     'sha256': hashlib.sha256(html.encode()).hexdigest()})
    with (DATA / 'fixtures.jsonl').open('w') as f:
        for row in rows:
            f.write(json.dumps(row) + '\n')
    # External benchmark unchanged; do not select pages on pagesift's own output.
    source = ROOT / 'bench/data/fresh/data/aeb'
    truth = json.loads((source / 'ground-truth.json').read_text())
    aeb = DATA / 'aeb'
    aeb.mkdir(exist_ok=True)
    with (DATA / 'aeb.jsonl').open('w') as out:
        for key, gold in truth.items():
            html = gzip.decompress((source / 'html' / f'{key}.html.gz').read_bytes())
            path = aeb / f'{key}.html'
            path.write_bytes(html)
            row = {'id': key, 'url': gold['url'], 'path': str(path),
                   'sha256': hashlib.sha256(html).hexdigest(), 'only_main': True}
            out.write(json.dumps(row) + '\n')
    print(json.dumps({'synthetic_images': len(rows), 'aeb': len(truth)}))


if __name__ == '__main__':
    main()

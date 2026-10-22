#!/usr/bin/env python3
"""Offline Scrapling selectors / gallery-dl's real GenericExtractor (no downloads)."""
import argparse
import json
from pathlib import Path
import time


def main():
    p = argparse.ArgumentParser()
    p.add_argument('tool', choices=['scrapling', 'gallery-dl'])
    p.add_argument('manifest')
    p.add_argument('output')
    a = p.parse_args()
    if a.tool == 'scrapling':
        from scrapling import Selector
    else:
        from gallery_dl.extractor.generic import GenericExtractor
        import re
    with open(a.output, 'w') as out:
        for row in map(json.loads, open(a.manifest)):
            html = Path(row['path']).read_text()
            url = row.get('base_url') or row.get('url') or 'https://example.com/'
            t = time.perf_counter()
            record = {'id': row['id'], 'status': 'ok'}
            try:
                if a.tool == 'scrapling':
                    page = Selector(html, url=url)
                    record['values'] = [n.get_all_text(separator=' ', strip=True) for n in page.css(row.get('selector', 'h1'))]
                    record['images'] = page.css('img::attr(src)').getall()
                else:
                    match = re.match(GenericExtractor.pattern, 'generic:' + url)
                    if not match:
                        raise ValueError('GenericExtractor does not accept this URL')
                    extractor = GenericExtractor(match)
                    record['images'] = [item[0] for item in extractor.images(html)]
                record['ms'] = (time.perf_counter() - t) * 1000
            except Exception as e:
                record.update(status='error', error=f'{type(e).__name__}: {e}', ms=(time.perf_counter() - t) * 1000)
            out.write(json.dumps(record, ensure_ascii=False) + '\n')
            out.flush()


if __name__ == '__main__':
    main()

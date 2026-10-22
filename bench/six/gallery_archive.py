#!/usr/bin/env python3
"""Exercise gallery-dl's real archive semantics across reopen and interruption."""
import json
from pathlib import Path
import sys
from gallery_dl.archive import DownloadArchive


def main():
    out = Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)
    db = out / 'archive.sqlite3'
    if db.exists():
        raise RuntimeError('use a new output directory; archive must start empty')
    keygen = lambda item: item['site'] + ':' + item['id']
    a = DownloadArchive(str(db), keygen)
    fresh = repeated = 0
    for i in range(20):
        item = {'site': 'controlled', 'id': str(i), 'url': f'https://example.com/{i}.jpg'}
        if not a.check(item): fresh += 1; a.add(item)
    a.close()
    a = DownloadArchive(str(db), keygen)
    for i in range(20):
        item = {'site': 'controlled', 'id': str(i), 'url': f'https://cdn.example.com/new-location/{i}.jpg'}
        if a.check(item): repeated += 1
    uncommitted = {'site': 'controlled', 'id': 'interrupted-before-add'}
    missing_before = not a.check(uncommitted)
    a.close()
    a = DownloadArchive(str(db), keygen)
    retry_after_reopen = not a.check(uncommitted)
    a.close()
    rec = {'kind': 'native archive semantic diagnostic, not an end-to-end downloader benchmark',
           'fresh_items': fresh, 'same_ids_skipped_after_reopen_and_url_change': repeated,
           'not_added_before_interrupt': missing_before, 'retried_after_reopen': retry_after_reopen,
           'pagesift_gap': 'No equivalent site-item ID archive API. URL/cache/content snapshots have different semantics.'}
    assert fresh == 20 and repeated == 20 and missing_before and retry_after_reopen
    (out / 'summary.json').write_text(json.dumps(rec, indent=2) + '\n')
    print(json.dumps(rec))


if __name__ == '__main__':
    main()

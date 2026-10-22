#!/usr/bin/env python3
"""Real gallery-dl archive and Rust store primitive; no site-extractor claims."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from gallery_dl.archive import DownloadArchive

ROOT=Path(__file__).resolve().parents[2]
BINARY=ROOT/'bench/data/six/rust-target/release/examples/item_archive'


def session(path):
    return subprocess.Popen([str(BINARY),str(path)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)


def op(p,action,source,item,revision=''):
    p.stdin.write('\t'.join((action,source,item,revision))+'\n');p.stdin.flush()
    line=p.stdout.readline().strip()
    if line not in ('true','false'): raise RuntimeError('archive worker failed')
    return line=='true'


def close(p):
    p.stdin.close()
    if p.wait(timeout=10):raise RuntimeError('archive close failed')


def main():
    out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
    # Same identity-only model: source+item ID, transport URL deliberately changes.
    a=DownloadArchive(str(out/'gallery.sqlite3'),lambda item:item['source']+':'+item['id'])
    p=session(out/'rust-store')
    try:
        for i in range(20):
            item={'source':'controlled','id':str(i),'url':f'https://example.com/{i}'}
            assert not a.check(item) and not op(p,'check',item['source'],item['id'])
            a.add(item);assert op(p,'commit',item['source'],item['id'])
    finally:a.close();close(p)
    a=DownloadArchive(str(out/'gallery.sqlite3'),lambda item:item['source']+':'+item['id'])
    p=session(out/'rust-store')
    counts={'gallery':0,'rust':0}
    try:
        for i in range(20):
            item={'source':'controlled','id':str(i),'url':f'https://cdn.example.net/new/{i}'}
            counts['gallery']+=bool(a.check(item))
            counts['rust']+=op(p,'check',item['source'],item['id'])
        assert not op(p,'check','controlled','pending')
        # Rust-only explicit version token mode; not compared to a default
        # gallery-dl identity archive as if the latter tracked content changes.
        assert not op(p,'check','controlled','0','revision-2')
        op(p,'commit','controlled','0','revision-2')
    finally:a.close()
    p.kill();p.wait(timeout=10)
    p=session(out/'rust-store')
    try:
        pending_retried=not op(p,'check','controlled','pending')
        version_saved=op(p,'check','controlled','0','revision-2')
        old_version_rejected=not op(p,'check','controlled','0','')
        op(p,'forget','controlled','1')
    finally:close(p)
    p=session(out/'rust-store')
    try:forgotten=not op(p,'check','controlled','1')
    finally:close(p)
    result={'scope':'native archive/store primitives; not crawler integration or real-site extraction',
            'identity_only_reopen_changed_url':counts,'n':20,'rust_uncommitted_item_retried_after_SIGKILL':pending_retried,
            'rust_fsynced_revision_survives_SIGKILL':version_saved,'rust_old_revision_rejected':old_version_rejected,
            'rust_forget_survives_reopen':forgotten,'rust_binary_sha256':hashlib.sha256(BINARY.read_bytes()).hexdigest(),
            'durability':'Rust SyncPolicy::Always; gallery normal close. Not a comparative power-loss or performance test.'}
    (out/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
    assert counts=={'gallery':20,'rust':20} and all((pending_retried,version_saved,old_version_rejected,forgotten))


if __name__=='__main__':main()

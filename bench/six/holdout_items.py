#!/usr/bin/env python3
"""New item IDs, repeated transport URLs, explicit versions; primitive scope only."""
import json,random,sys
from pathlib import Path
from gallery_dl.archive import DownloadArchive
import item_identity as rust
ROOT=Path(__file__).resolve().parents[2]


def main():
    out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
    protocol=json.loads((ROOT/'bench/six/results/round4/protocol-v2.json').read_text());seed=protocol['master_seed'];rng=random.Random(seed)
    source='catalog:'+str(seed);ids=['条目-'+str(i)+'-'+str(rng.randrange(100000)) for i in range(31)]
    rust.BINARY=ROOT/'bench/data/six/acceptance/source-v2/target/release/examples/item_archive'
    archive=DownloadArchive(str(out/'native.sqlite3'),lambda row:row['source']+':'+row['id']);p=rust.session(out/'rust')
    try:
        for key in ids:
            row={'source':source,'id':key,'url':'https://example.com/shared-transport'}
            assert not archive.check(row) and not rust.op(p,'check',source,key)
            archive.add(row);rust.op(p,'commit',source,key)
    finally:archive.close();rust.close(p)
    archive=DownloadArchive(str(out/'native.sqlite3'),lambda row:row['source']+':'+row['id']);p=rust.session(out/'rust')
    counts={'gallery':0,'rust':0}
    try:
        for key in ids:
            row={'source':source,'id':key,'url':'https://cdn.example.net/new/'+str(rng.randrange(999999))}
            counts['gallery']+=bool(archive.check(row));counts['rust']+=rust.op(p,'check',source,key)
        versioned=rng.sample(ids,7)
        for key in versioned:
            assert not rust.op(p,'check',source,key,'revision-two')
            rust.op(p,'commit',source,key,'revision-two')
        assert not rust.op(p,'check',source,'uncommitted')
    finally:archive.close();rust.close(p)
    p=rust.session(out/'rust')
    try:
        updates=sum(rust.op(p,'check',source,key,'revision-two') for key in versioned)
        pending=not rust.op(p,'check',source,'uncommitted')
        separate_source=not rust.op(p,'check','different-source',ids[0])
    finally:rust.close(p)
    result={'n':31,'identity_reopen_changed_url':counts,'explicit_versions_persisted':updates,'versions_tested':7,
        'uncommitted_remains_pending':pending,'source_isolation':separate_source,
        'scope':'native archive and Rust storage API; no real-site extractor or automatic crawler identity integration'}
    (out/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
    assert counts=={'gallery':31,'rust':31} and updates==7 and pending and separate_source


if __name__=='__main__':main()

#!/usr/bin/env python3
"""Freeze product sources in a separate Git clone, then cold-build and run CI."""
import hashlib,json,os
from pathlib import Path
import shutil,subprocess,sys

ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4'
LABEL=sys.argv[1] if len(sys.argv)>1 else ''
CLONE=ROOT/('bench/data/six/acceptance/source'+LABEL)
NEW=['crates/pagesift/src/discovery.rs','crates/pagesift-store/src/items.rs',
     'crates/pagesift-store/examples/item_archive.rs','docs/comparison-plan-2026-10-22.md']


def main():
    if CLONE.exists():raise RuntimeError('acceptance clone must start absent')
    OUT.mkdir(parents=True,exist_ok=True)
    patch=subprocess.check_output(['git','diff','--binary','HEAD'],cwd=ROOT)
    (OUT/('tracked-candidate'+LABEL+'.patch')).write_bytes(patch)
    base=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()
    subprocess.run(['git','clone','--no-hardlinks',str(ROOT),str(CLONE)],check=True)
    subprocess.run(['git','apply','--binary','-'],input=patch,cwd=CLONE,check=True)
    for name in NEW:
        dest=CLONE/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(ROOT/name,dest)
    subprocess.run(['git','add','-A'],cwd=CLONE,check=True)
    subprocess.run(['git','-c','user.name=Acceptance Snapshot','-c','user.email=acceptance@example.invalid',
                    '-c','core.hooksPath=/dev/null','commit','--no-gpg-sign','-m','Freeze local candidate for independent acceptance'],cwd=CLONE,check=True)
    sha=subprocess.check_output(['git','rev-parse','HEAD'],cwd=CLONE,text=True).strip()
    assert not subprocess.check_output(['git','status','--porcelain'],cwd=CLONE)
    sources={name:hashlib.sha256((CLONE/name).read_bytes()).hexdigest() for name in subprocess.check_output(['git','ls-files'],cwd=CLONE,text=True).splitlines()}
    state={'base_sha':base,'candidate_sha':sha,'clone':str(CLONE),'cold_target':str(CLONE/'target'),
           'tracked_patch_sha256':hashlib.sha256(patch).hexdigest(),'source_hashes':sources,
           'snapshot_commit_scope':'temporary acceptance clone only; original branch and index untouched',
           'status':'frozen'}
    def save(): (OUT/('build'+LABEL+'.json')).write_text(json.dumps(state,indent=2)+'\n')
    save()
    env=dict(os.environ,CARGO_TARGET_DIR=str(CLONE/'target'),CARGO_PROFILE_DEV_DEBUG='0',CARGO_PROFILE_TEST_DEBUG='0',CARGO_INCREMENTAL='0',RUST_TEST_THREADS='4')
    subprocess.run(['bash','scripts/ci.sh'],cwd=CLONE,env=env,check=True)
    subprocess.run(['cargo','build','--release','--locked','-p','pagesift','--bins','--examples'],cwd=CLONE,env=env,check=True)
    subprocess.run(['cargo','build','--release','--locked','-p','pagesift-content','--examples'],cwd=CLONE,env=env,check=True)
    subprocess.run(['cargo','build','--release','--locked','-p','pagesift-store','--example','item_archive'],cwd=CLONE,env=env,check=True)
    assert not subprocess.check_output(['git','status','--porcelain'],cwd=CLONE)
    state.update(status='completed',binary=str(CLONE/'target/release/pagesift'),
        binary_sha256=hashlib.sha256((CLONE/'target/release/pagesift').read_bytes()).hexdigest(),
        source_unchanged=all(hashlib.sha256((CLONE/p).read_bytes()).hexdigest()==h for p,h in sources.items()))
    assert state['source_unchanged'];save();print(json.dumps({k:v for k,v in state.items() if k!='source_hashes'}))


if __name__=='__main__':main()

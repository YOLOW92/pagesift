#!/usr/bin/env python3
"""Snapshot product changes in an isolated clone; never commit the working repo."""
import hashlib,json,subprocess
from pathlib import Path
import shutil
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/capabilities'
CLONE=ROOT/'bench/data/six/capability-source'
if CLONE.exists(): raise RuntimeError('snapshot destination already exists')
OUT.mkdir(parents=True,exist_ok=True)
base=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip()
patch=subprocess.check_output(['git','diff','--binary','HEAD'],cwd=ROOT)
(OUT/'source.patch').write_bytes(patch)
subprocess.run(['git','clone','--no-hardlinks',str(ROOT),str(CLONE)],check=True)
subprocess.run(['git','apply','--binary','-'],cwd=CLONE,input=patch,check=True)
new=subprocess.check_output(['git','ls-files','--others','--exclude-standard','crates','docs'],cwd=ROOT,text=True).splitlines()
for name in new:
    dest=CLONE/name;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(ROOT/name,dest)
subprocess.run(['git','add','-A'],cwd=CLONE,check=True)
subprocess.run(['git','-c','user.name=Local Capability Snapshot','-c','user.email=snapshot@example.invalid','-c','core.hooksPath=/dev/null','commit','--no-gpg-sign','-m','Freeze local capability implementation for review'],cwd=CLONE,check=True)
sha=subprocess.check_output(['git','rev-parse','HEAD'],cwd=CLONE,text=True).strip()
files=subprocess.check_output(['git','ls-files'],cwd=CLONE,text=True).splitlines()
hashes={n:hashlib.sha256((CLONE/n).read_bytes()).hexdigest() for n in files}
archive=OUT/'candidate-source.tar.gz'
subprocess.run(['git','archive','--format=tar.gz','--output='+str(archive),sha],cwd=CLONE,check=True)
(OUT/'source.json').write_text(json.dumps({'base_sha':base,'snapshot_sha':sha,'clone':str(CLONE),'source_hashes':hashes,'archive_sha256':hashlib.sha256(archive.read_bytes()).hexdigest(),'scope':'temporary clone only; original index and branch untouched'},indent=2)+'\n')
print(sha)

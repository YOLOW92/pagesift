#!/usr/bin/env python3
"""Remove only this task's inactive build caches and five disposable images."""
import datetime,json,re,shutil,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
OUT=ROOT/'bench/six/results/round4'


def main():
    before=shutil.disk_usage('/').free
    assert not subprocess.check_output(['docker','ps','-q','--filter','name=pagesift-six-firecrawl'],text=True).strip()
    state=json.loads((OUT/'build-v2.json').read_text());clone=Path(state['clone'])
    assert not subprocess.check_output(['git','status','--porcelain'],cwd=clone)
    subprocess.run(['git','archive','--format=tar.gz','-o',str(OUT/'candidate-source.tar.gz'),state['candidate_sha']],cwd=clone,check=True)
    # Preserve both hashed perf_gate binaries: two crates share this example name.
    perf=json.loads((OUT/'perf-verified/summary.json').read_text())
    keep={Path(v['path']).name for v in perf['binaries'].values()}
    removed=[]
    for target in [ROOT/'bench/data/six/rust-target',ROOT/'bench/data/six/acceptance/source/target',ROOT/'bench/data/six/acceptance/source-v2/target']:
        for rel in ['debug','release/deps','release/build','release/.fingerprint','release/incremental']:
            p=target/rel
            if p.exists():shutil.rmtree(p);removed.append(str(p))
        examples=target/'release/examples'
        if examples.exists():
            for p in examples.iterdir():
                stem=p.name[:-2] if p.name.endswith('.d') else p.name
                if re.fullmatch(r'.+-[0-9a-f]{16}',stem) and stem not in keep:
                    p.unlink();removed.append(str(p))
    runtime=json.loads((ROOT/'bench/six/locks/firecrawl-runtime.json').read_text());images=[]
    for v in runtime.values():
        proc=subprocess.run(['docker','image','rm',v['image']],text=True,capture_output=True)
        images.append({'image':v['image'],'exit':proc.returncode,'stdout':proc.stdout,'stderr':proc.stderr})
    result={'time_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'free_bytes_before':before,'free_bytes_after':shutil.disk_usage('/').free,
      'removed':removed,'images':images,'remaining_images':subprocess.check_output(['docker','image','ls','--format','{{.Repository}}:{{.Tag}} {{.ID}}'],text=True),
      'preserved':'source clones and their exact Git commits, named executables, both hashed performance gates, all result records and input corpora'}
    (OUT/'cleanup.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({'freed_gib':(result['free_bytes_after']-before)/2**30,'remaining':result['remaining_images']}))


if __name__=='__main__':main()

#!/usr/bin/env python3
"""Serial legacy-domain regression with original scorers and isolated outputs."""
import hashlib,json,os,subprocess,sys
from pathlib import Path

ROOT=Path(__file__).resolve().parents[2]
CLONE=Path(sys.argv[2]).resolve() if len(sys.argv)>2 else ROOT/'bench/data/six/acceptance/source'
OUT=Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'bench/six/results/round4/domains'
FRESH=ROOT/'bench/data/fresh'
PY=FRESH/'tools/py/bin/python'
C4=FRESH/'tools/c4ai/bin/python'
BASE=ROOT/'target/release/examples'
CAND=CLONE/'target/release/examples'


def run(args,log,cwd=ROOT,env=None):
    with Path(log).open('w') as f:
        command=list(map(str,args))
        if command[0] in (str(PY),str(C4),sys.executable): command[1:1]=['-X','cpu_count=8']
        subprocess.run(command,cwd=cwd,env=env,stdout=f,stderr=subprocess.STDOUT,check=True,timeout=900)


def main():
    OUT.mkdir(parents=True,exist_ok=False)
    shared=CLONE/'bench/data/fresh';shared.parent.mkdir(parents=True,exist_ok=True)
    if not shared.exists():shared.symlink_to(FRESH,target_is_directory=True)
    summary={};protocol=json.loads((Path(sys.argv[3]) if len(sys.argv)>3 else ROOT/'bench/six/results/round4/protocol.json').read_text())
    inputs={}
    def capture(path):
        path=Path(path);inputs[str(path)]=hashlib.sha256(path.read_bytes()).hexdigest()
    # Fresh random structural documents. Existing GFM round-trip scorer unchanged.
    md=OUT/'markdown';md.mkdir();summary['markdown']=[]
    for seed in protocol['markdown_seeds']:
        d=md/str(seed);d.mkdir();html=d/'html'
        run([C4,'-I',CLONE/'bench/markdown/fidelity.py','gen',html,200,seed],d/'generate.log')
        for name,binary in [('baseline',BASE/'md_convert'),('candidate',CAND/'md_convert')]:
            run([binary,html,d/name,'--full'],d/f'{name}.log')
        # Essence component is explicitly full conversion, not end-to-end fetching.
        manifest=d/'manifest.jsonl'
        rows=[{'id':p.stem,'path':str(p),'only_main':False,'base_url':'https://example.com/'} for p in sorted(html.glob('*.html'))]
        manifest.write_text(''.join(json.dumps(r)+'\n' for r in rows))
        run([ROOT/'bench/data/six/bin/offline-stable','essence',manifest,d/'essence.jsonl'],d/'essence.log')
        essence=d/'essence';essence.mkdir()
        for r in map(json.loads,(d/'essence.jsonl').read_text().splitlines()):
            if r['status']!='ok':raise RuntimeError('Essence component error')
            (essence/(r['id']+'.md')).write_text(r['markdown'])
        run([C4,'-I',CLONE/'bench/markdown/fidelity.py','score',html,f'baseline={d/"baseline"}',f'candidate={d/"candidate"}',f'essence-component={essence}'],d/'score.log')
        same=sum((d/'baseline'/p.name).read_bytes()==p.read_bytes() for p in (d/'candidate').glob('*.md'))
        summary['markdown'].append({'seed':seed,'n':200,'byte_identical':same,'score':str((d/'score.log').relative_to(ROOT))})
        for p in html.rglob('*'):
            if p.is_file():capture(p)
    # Original htmldate dataset + scorer, both URL modes.
    dates=OUT/'dates';dates.mkdir()
    for with_url in (False,True):
        name='with-url' if with_url else 'without-url'
        run([PY,'-I',CLONE/'bench/meta/dates.py','--tools','pagesift','--dump',dates/(name+'.json'),*(['--url'] if with_url else [])],dates/(name+'.log'),cwd=CLONE)
        d=json.loads((dates/(name+'.json')).read_text());pages=d['pages']
        lst=dates/(name+'.txt');lst.write_text(''.join(p['path']+('\t'+p['url'] if with_url else '')+'\n' for p in pages))
        run([BASE/'meta_eval',lst],dates/(name+'-baseline.jsonl'))
        before={r['file']:((r.get('published_local') or r.get('published') or '')[:10] or None) for r in map(json.loads,(dates/(name+'-baseline.jsonl')).read_text().splitlines())}
        after=d['results']['pagesift'];same=sum(before[p['path']]==after[p['path']] for p in pages)
        summary['dates-'+name]={'n':len(pages),'predictions_identical':same,'score':str((dates/(name+'.log')).relative_to(ROOT))}
        for p in pages:capture(p['path'])
    # RAG/query filter and chunking on both established datasets, original scorers.
    rag=OUT/'rag';rag.mkdir();summary['rag']={}
    for dataset in ('chunkeval','qasper-eval'):
        data=FRESH/'data'/dataset;d=rag/dataset;d.mkdir();chunks=d/'chunks';chunks.mkdir()
        q=d/'questions.jsonl';run([sys.executable,CLONE/'bench/rag/qf_prepare.py',data,q],d/'prepare.log')
        for name,examples in [('baseline',BASE),('candidate',CAND)]:
            run([examples/'qf_ranks',data/'corpora',q,d/(name+'.json')],d/(name+'-qf.log'))
            run([examples/'chunk_spans','{"type":"recursive","size":200}',chunks/(name+'.json'),*sorted((data/'corpora').glob('*.md'))],d/(name+'-chunks.log'))
        run([sys.executable,CLONE/'bench/rag/qf_eval.py',data/'corpora',q,d/'baseline.json',d/'candidate.json'],d/'query-filter-score.log')
        run([PY,'-I',CLONE/'bench/rag/eval.py',data,chunks],d/'chunking-score.log')
        summary['rag'][dataset]={'questions':len(q.read_text().splitlines()),
          'query_filter_identical':json.loads((d/'baseline.json').read_text())==json.loads((d/'candidate.json').read_text()),
          'chunk_spans_identical':json.loads((chunks/'baseline.json').read_text())==json.loads((chunks/'candidate.json').read_text())}
        capture(data/'questions_df.csv')
        for p in (data/'corpora').glob('*.md'):capture(p)
    # All five electronic PDF subsets, original native benchmark with existing math prefilter.
    pdf=OUT/'pdf';pdf.mkdir();data=FRESH/'data/olmocr-bench'
    (pdf/'pdfs').symlink_to(data/'pdfs',target_is_directory=True)
    for p in data.glob('*.jsonl'):(pdf/p.name).symlink_to(p);capture(p)
    for name,binary in [('baseline',BASE/'pdf_dir'),('candidate',CAND/'pdf_dir')]:
        run([binary,data/'pdfs',pdf/name],pdf/(name+'-convert.log'))
    files=sorted((pdf/'candidate').rglob('*.md'))
    same=sum((pdf/'baseline'/p.relative_to(pdf/'candidate')).read_bytes()==p.read_bytes() for p in files)
    env=dict(os.environ,PYTHONPATH=str(FRESH/'tools/olmocr-bench'),PYTHONDONTWRITEBYTECODE='1')
    run([PY,CLONE/'bench/pdf/bench_fast.py','--dir',pdf,'--candidate','candidate','--bootstrap_samples','200'],pdf/'score.log',cwd=FRESH/'tools/olmocr-bench',env=env)
    summary['pdf']={'n':len(files),'markdown_identical':same,'subsets':sorted(p.stem for p in data.glob('*.jsonl')),'score':str((pdf/'score.log').relative_to(ROOT))}
    for p in (data/'pdfs').rglob('*.pdf'):capture(p)
    (OUT/'input-hashes.json').write_text(json.dumps(inputs,indent=2)+'\n')
    (OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
    assert all(r['byte_identical']==r['n'] for r in summary['markdown'])
    assert all(v['predictions_identical']==v['n'] for k,v in summary.items() if k.startswith('dates-'))
    assert all(v['query_filter_identical'] and v['chunk_spans_identical'] for v in summary['rag'].values())
    assert same==len(files)


if __name__=='__main__':main()

#!/usr/bin/env python3
"""Frozen exact-selector / native adaptive relocation capability calibration."""
import hashlib,json,subprocess,sys,tempfile
from pathlib import Path
from scrapling import Selector
ROOT=Path(__file__).resolve().parents[2]


def main():
    out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
    seed=json.loads((ROOT/'bench/six/results/round4/protocol-v2.json').read_text())['master_seed']
    key='record-'+str(seed);name='Independent Widget 東京';price='Cost 73'
    old=f'<main><section id="{key}" class="product" data-item-id="inventory-A"><h2>{name}</h2><p>{price}</p></section></main>'
    expected=name+' '+price
    other=old.replace('inventory-A','inventory-B')
    changed=old.replace(f'id="{key}"',f'data-identity="{key}"')
    cases=[('unchanged',old,expected),('class-change',old.replace('class="product"','class="entry"'),expected),
      ('id-relocated',changed,expected),('wrapper-relocated','<div class="layout"><aside>Menu</aside>'+changed+'</div>',expected),
      ('moved-parent','<article>'+changed.replace('<main>','').replace('</main>','')+'</article>',expected),
      ('removed','<main><p>No record remains</p></main>',None),
      ('same-class-price',other.replace(key,'other').replace(name,'Different Widget 東京'),None),
      ('same-name-other-price',other.replace(key,'other').replace(price,'Cost 99'),None),
      ('empty','<main></main>',None),
      ('two-lookalikes',other.replace(key,'first').replace(name,'Other A')+other.replace(key,'second').replace(name,'Other B').replace('inventory-B','inventory-C'),None),
      ('unrelated-menu','<nav><a href="/">Independent Widget 東京</a></nav>',None),
      ('unicode-wrapper','<main><p>説明</p>'+old+'</main>',expected)]
    manifest=[];details=[]
    (out/'original.html').write_text(old)
    with tempfile.TemporaryDirectory(prefix='pagesift-acceptance-relocation-') as tmp:
        opts={'url':'https://example.com/'+str(seed)+'/','adaptive':True,'storage_args':{'storage_file':str(Path(tmp)/'elements.db')}}
        Selector(old,**opts).css('#'+key,auto_save=True)
        for ident,text,gold in cases:
            p=out/(ident+'.html');p.write_text(text)
            row={'id':ident,'path':str(p),'selector':'#'+key,'gold':gold,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()};manifest.append(row)
            page=Selector(text,**opts);nodes=page.css('#'+key)
            if not nodes:nodes=page.css('#'+key,adaptive=True)
            values=[n.get_all_text(separator=' ',strip=True) for n in nodes]
            details.append({'id':ident,'gold':gold,'scrapling_values':values})
    m=out/'manifest.jsonl';m.write_text(''.join(json.dumps(r)+'\n' for r in manifest))
    binary=ROOT/'bench/data/six/acceptance/source-v2/target/release/pagesift-acceptance-selector'
    subprocess.run([str(binary),str(m),str(out/'pagesift.jsonl')],check=True,timeout=60)
    before={r['id']:r for r in map(json.loads,(out/'pagesift.jsonl').read_text().splitlines())}
    norm=lambda values:[''.join(v.split()) for v in values]
    for row in details:
        row['pagesift_values']=before[row['id']]['values'];wanted=[] if row['gold'] is None else [row['gold']]
        for tool in ('scrapling','pagesift'):row[tool+'_correct']=norm(row[tool+'_values'])==norm(wanted)
    summary={'n':len(details),'details':details,'identity_gold':'inventory-A is the intended item; lookalikes carry B/C or no item ID; DOM id relocation alone does not change business identity',
      'scope':'exact native Rust selector versus default native adaptive relocation; synthetic capability cases, not population accuracy',
      'pagesift':{'correct':sum(r['pagesift_correct'] for r in details),'adaptive_implemented':False},
      'scrapling':{'correct':sum(r['scrapling_correct'] for r in details),'negative_false_matches':sum(r['gold'] is None and bool(r['scrapling_values']) for r in details)},
      'frozen_selector_probe_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()}
    (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps({k:v for k,v in summary.items() if k!='details'}))


if __name__=='__main__':main()

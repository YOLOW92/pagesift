import sys,os,collections
import pymupdf
from PIL import Image
tsv, imgdir, srcdir = sys.argv[1:4]
ours=collections.defaultdict(list)
for l in open(tsv):
    f=l.rstrip('\n').split('\t')
    if f[0]!='image' or f[3]!='pixels': continue
    w,h=map(int,f[4].split('x')); ours[f[1]].append((w,h,f[2],f[6]))
stats=collections.Counter(); diffs=[]
for doc,lst in ours.items():
    try: d=pymupdf.open(os.path.join(srcdir,doc+'.pdf'))
    except Exception: continue
    xrefs={}
    for p in d:
        for im in p.get_images(full=True): xrefs[im[0]]=im
    for w,h,name,cs in lst:
        cands=[x for x,im in xrefs.items() if im[2]==w and im[3]==h]
        if len(cands)!=1: stats['ambiguous']+=1; continue
        try:
            pm=pymupdf.Pixmap(d,cands[0])
            if pm.alpha: pm=pymupdf.Pixmap(pm,0)
            if pm.n!=3 or pm.colorspace is None or pm.colorspace.n!=3: pm=pymupdf.Pixmap(pymupdf.csRGB,pm)
            ref=Image.frombytes('RGB',(pm.width,pm.height),pm.samples)
        except Exception as e: stats['mupdf_err']+=1; continue
        mine=Image.open(os.path.join(imgdir,name)).convert('RGB')
        a=ref.tobytes(); b=mine.tobytes()
        if len(a)!=len(b): stats['size']+=1; continue
        md=sum(abs(x-y) for x,y in zip(a[:300000],b[:300000]))/min(len(a),300000)
        k='exact' if md==0 else ('close' if md<8 else 'differs')
        stats[k]+=1
        if k=='differs': diffs.append((doc,name,cs,round(md,1)))
print(stats); print(diffs[:20])

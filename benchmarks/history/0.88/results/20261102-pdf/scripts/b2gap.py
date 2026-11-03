import re,sys,subprocess,unicodedata,os,statistics
from collections import Counter
W=re.compile(r"\w+",re.U)
def words(s): return Counter(W.findall(unicodedata.normalize("NFKC",s).casefold()))
GREEK={'alpha':'α','beta':'β','gamma':'γ','delta':'δ','epsilon':'ε','varepsilon':'ε','zeta':'ζ','eta':'η','theta':'θ','vartheta':'ϑ','iota':'ι','kappa':'κ','lambda':'λ','mu':'μ','nu':'ν','xi':'ξ','pi':'π','rho':'ρ','sigma':'σ','tau':'τ','upsilon':'υ','phi':'φ','varphi':'φ','chi':'χ','psi':'ψ','omega':'ω','Gamma':'Γ','Delta':'Δ','Theta':'Θ','Lambda':'Λ','Xi':'Ξ','Pi':'Π','Sigma':'Σ','Phi':'Φ','Psi':'Ψ','Omega':'Ω'}
def demath(md):
    def f(m):
        t=m.group(1)
        t=re.sub(r'\\([A-Za-z]+)\s*', lambda g: GREEK.get(g.group(1),' '), t)
        t=re.sub(r'[_^]\{([^{}]*)\}', r'\1', t); t=re.sub(r'[_^](\w)', r'\1', t)
        t=t.replace('{','').replace('}','')
        return t
    return re.sub(r'\$([^$]*)\$', f, md)
Q,I=sys.argv[1],sys.argv[2]
a=[];b=[]
for fn in sorted(os.listdir(I)):
    d=fn[:-4]; ref=words(subprocess.run(["pdftotext","-enc","UTF-8",f"{I}/{fn}","-"],capture_output=True).stdout.decode("utf-8","replace"))
    tot=sum(ref.values())
    if tot<50: continue
    p=f"{Q}/{d}.md"; md=open(p,errors='replace').read() if os.path.exists(p) else ''
    for lst,txt in ((a,md),(b,demath(md))):
        ow=words(txt); lst.append(sum(min(c,ow[w]) for w,c in ref.items())/tot)
print('原样',statistics.mean(a),'数学还原后',statistics.mean(b),len(a))

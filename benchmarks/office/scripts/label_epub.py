#!/usr/bin/env python3
"""EPUB 真值：按 OPF spine 顺序读每个 XHTML（规则写在 REVIEW.md）：
- 标题：h1–h6 的文字节点（不含图片替代文字），级别照原样；
- 表格：<table>，尺寸按 colspan 展开，去掉全空的表；
- 列表：nav 文档以外的 <li>（只取 li 自己的文字，不含嵌套列表）；
- 图片替代文字：2 个字符以上的 alt；脚注：epub:type 为 footnote/endnote/rearnote 的元素，
  没有时保留 pandoc 的候选；正文样句：40 字以上的 <p>，全书均匀取 5 句。
用法：label_epub.py <truth 目录> <epub...>"""
import html.parser, json, os, posixpath, re, sys, unicodedata, urllib.parse, zipfile
import xml.etree.ElementTree as ET
def norm(s): return re.sub(r"\s+", " ", unicodedata.normalize("NFKC", s)).strip()
def spread(xs, n): return xs if len(xs) <= n else [xs[i * len(xs) // n] for i in range(n)]
class X(html.parser.HTMLParser):
    def __init__(s, nav):
        super().__init__(convert_charrefs=True); s.nav=nav; s.h=[]; s.tables=[]; s.lists=[]; s.alts=[]; s.notes=[]; s.paras=[]
        s.stack=[]
    def handle_starttag(s,t,a):
        a=dict(a)
        if t in ("h1","h2","h3","h4","h5","h6","li","p","td","th","table","tr"): s.stack.append([t,a,[],[]])
        if t in ("aside","section","div","ol") and re.search(r"footnote|endnote|rearnote", a.get("epub:type","")): s.stack.append([t,a,[],[]])
        if t=="img" and len(norm(a.get("alt") or ""))>=2: s.alts.append(norm(a["alt"]))
        if t=="br": s.handle_data(" ")
    def handle_endtag(s,t):
        for i in range(len(s.stack)-1,-1,-1):
            if s.stack[i][0]==t: break
        else: return
        tag,a,buf,rows=s.stack.pop(i); del s.stack[i:]
        text=norm("".join(buf))
        if re.search(r"footnote|endnote|rearnote", a.get("epub:type","")):
            if text and tag!="ol": s.notes.append(text[:80])
            return
        if tag[0]=="h" and tag[1:].isdigit() and text: s.h.append([int(tag[1]),text])
        elif tag=="li" and text and not s.nav: s.lists.append(text[:80])
        elif tag=="p" and len(text)>=40: s.paras.append(text[:80])
        elif tag in ("td","th"):
            for x in s.stack[::-1]:
                if x[0]=="tr": x[3].extend([text]+[""]*(int(a.get("colspan") or 1)-1)); break
        elif tag=="tr":
            for x in s.stack[::-1]:
                if x[0]=="table": x[3].append(rows); break
        elif tag=="table":
            rows=[r for r in rows if r]
            if rows and any(c for r in rows for c in r):
                cells=[c for r in rows for c in r if 0<len(c)<=60]; pick=[]
                for c in cells[:2]+cells[len(cells)//2:len(cells)//2+1]+cells[-1:]:
                    if c not in pick: pick.append(c)
                s.tables.append({"rows":len(rows),"cols":max(map(len,rows)),"cells":pick})
    def handle_data(s,d):
        for x in s.stack:
            if x[0]!="li" or x is s.stack[-1] or not any(y[0]=="li" for y in s.stack[s.stack.index(x)+1:]): x[2].append(d)
out=sys.argv[1]
for path in sys.argv[2:]:
    z=zipfile.ZipFile(path)
    opf=ET.fromstring(z.read("META-INF/container.xml")).find(".//{*}rootfile").get("full-path")
    root=ET.fromstring(z.read(opf)); base=posixpath.dirname(opf)
    man={i.get("id"):(i.get("href"),i.get("properties") or "") for i in root.iter("{http://www.idpf.org/2007/opf}item")}
    x=None; H=[];T=[];L=[];A=[];N=[];P=[]
    for ref in root.iter("{http://www.idpf.org/2007/opf}itemref"):
        href,props=man.get(ref.get("idref"),(None,""))
        if not href: continue
        name=posixpath.normpath(posixpath.join(base,urllib.parse.unquote(href)))
        try: data=z.read(name).decode("utf-8","replace")
        except KeyError: continue
        x=X("nav" in props.split()); x.feed(data)
        H+=x.h;T+=x.tables;L+=x.lists;A+=x.alts;N+=x.notes;P+=x.paras
    f=os.path.join(out,os.path.basename(path)+".json"); j=json.load(open(f))
    j.update(headings=spread(H,30),tables=spread(T,10),lists=spread(L,20),images=spread(A,10),text=spread(P,5),order=[h[1] for h in spread(H,8)])
    if N: j["footnotes"]=spread(N,10)
    json.dump(j,open(f,"w"),ensure_ascii=False,indent=1)
    print(os.path.basename(path),len(H),"标题",len(T),"表",len(L),"列表",len(A),"图",len(N),"注")

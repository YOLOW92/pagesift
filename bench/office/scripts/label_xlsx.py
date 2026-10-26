#!/usr/bin/env python3
"""xlsx 的真值：LibreOffice Calc（en-US 区域）导出的 HTML 里是单元格的显示值（按数字格式），
比 pandoc 的原始数值更接近用户看到的内容。每个工作表一个标题（表名）和一张表；
合并单元格按 rowspan/colspan 展开，表的尺寸是去掉首尾全空行、全空列后的范围。
表的尺寸（2026-10-27 留出集评分后改，见 REVIEW.md）直接从原始工作表 XML 算：有内容、没隐藏的单元格所在的
行、列范围，范围内隐藏的行列不计。LibreOffice 的 HTML 会把图片画成跨行跨列的单元格，从排版反推尺寸不可靠。
用法：label_xlsx.py <lo html 目录> <truth 目录>"""
import html.parser, json, os, re, sys, unicodedata
def norm(s): return re.sub(r"\s+", " ", unicodedata.normalize("NFKC", s)).strip()
class P(html.parser.HTMLParser):
    def __init__(s):
        super().__init__(); s.sheets=[]; s.h1=None; s.grid=None; s.cell=None; s.row=-1; s.span={}; s.loose=[]
    def handle_starttag(s,t,a):
        a=dict(a)
        if t=="h1": s.h1=[]
        if t=="table": s.grid={}; s.row=-1; s.span={}
        if t=="tr" and s.grid is not None: s.row+=1; s.col=0
        if t=="td" and s.grid is not None:
            while (s.row,s.col) in s.grid: s.col+=1
            s.cell=([], int(a.get("rowspan") or 1), int(a.get("colspan") or 1))
        if t=="br" and s.cell: s.cell[0].append(" ")
    def handle_endtag(s,t):
        if t=="h1" and s.h1 is not None:
            m=re.match(r"Sheet \d+: (.*)", norm("".join(s.h1)))
            if m: s.sheets.append([m.group(1), {}])
            s.h1=None
        if t=="td" and s.cell:
            text=norm("".join(s.cell[0])); rs,cs=s.cell[1],s.cell[2]
            for i in range(rs):
                for j in range(cs): s.grid[(s.row+i,s.col+j)]=text if (i,j)==(0,0) else ""
            s.col+=cs; s.cell=None
        if t=="table" and s.grid is not None:
            if s.sheets: s.sheets[-1][1]=s.grid
            else: s.loose.append(s.grid)  # 只有一个工作表时 LibreOffice 不写表名
            s.grid=None
    def handle_data(s,d):
        if s.h1 is not None: s.h1.append(d)
        if s.cell: s.cell[0].append(d)
def xml_dims(xlsx):
    """原始 xlsx 里每个工作表（按名字）的 (行数, 列数)：有内容、没隐藏的单元格的范围，扣掉范围内隐藏的行列。"""
    import zipfile, html as H
    z = zipfile.ZipFile(xlsx); names = set(z.namelist())
    def rd(n): return z.read(n).decode("utf-8", "replace")
    rels = {m.group(1): m.group(2) for m in re.finditer(r'<Relationship [^>]*?Id="([^"]+)"[^>]*?Target="([^"]+)"', rd("xl/_rels/workbook.xml.rels"))}
    rels.update({m.group(2): m.group(1) for m in re.finditer(r'<Relationship [^>]*?Target="([^"]+)"[^>]*?Id="([^"]+)"', rd("xl/_rels/workbook.xml.rels"))})
    ss = []
    if "xl/sharedStrings.xml" in names:
        for si in re.findall(r"<si>(.*?)</si>", rd("xl/sharedStrings.xml"), re.S):
            si = re.sub(r"<rPh\b.*?</rPh>", "", si, flags=re.S)
            ss.append(H.unescape("".join(re.findall(r"<t[^>]*>(.*?)</t>", si, re.S))))
    def ci(c):
        n = 0
        for ch in c: n = n * 26 + ord(ch) - 64
        return n
    out = {}
    for m in re.finditer(r"<sheet\b([^>]*)/?>", rd("xl/workbook.xml")):
        a = m.group(1); name = re.search(r'name="([^"]*)"', a); rid = re.search(r'r:id="([^"]*)"', a)
        if not name or not rid or rid.group(1) not in rels: continue
        t = rels[rid.group(1)]; part = t.lstrip("/") if t.startswith("/") else "xl/" + t
        if part not in names: continue
        x = rd(part)
        hc = set()
        for c in re.finditer(r"<col\b([^>]*)/?>", x):
            if re.search(r'hidden="(1|true)"', c.group(1)):
                lo = int(re.search(r'min="(\d+)"', c.group(1)).group(1)); hi = int(re.search(r'max="(\d+)"', c.group(1)).group(1))
                hc.update(range(lo, min(hi, 20000) + 1))
        hr = {int(r) for r in re.findall(r'<row\b[^>]*?r="(\d+)"[^>]*?hidden="(?:1|true)"', x)}
        hr |= {int(r) for r in re.findall(r'<row\b[^>]*?hidden="(?:1|true)"[^>]*?r="(\d+)"', x)}
        filled = []
        for c in re.finditer(r'<c\b[^>]*?r="([A-Z]+)(\d+)"([^>]*?)(?:/>|>(.*?)</c>)', x, re.S):
            col, row, attr, body = ci(c.group(1)), int(c.group(2)), c.group(3), c.group(4) or ""
            if row in hr or col in hc: continue
            if 't="inlineStr"' in attr: v = "".join(re.findall(r"<t[^>]*>(.*?)</t>", body, re.S))
            else:
                v = re.search(r"<v>(.*?)</v>", body, re.S); v = v.group(1) if v else ""
                if 't="s"' in attr and v.strip().isdigit() and int(v) < len(ss): v = ss[int(v)]
            if H.unescape(v).strip(): filled.append((row, col))
        if not filled: continue
        r0 = min(r for r, _ in filled); r1 = max(r for r, _ in filled); c0 = min(c for _, c in filled); c1 = max(c for _, c in filled)
        out[H.unescape(name.group(1))] = (r1 - r0 + 1 - len([r for r in hr if r0 <= r <= r1]), c1 - c0 + 1 - len([c for c in hc if c0 <= c <= c1]))
    return out
def spread(xs,n): return xs if len(xs)<=n else [xs[i*len(xs)//n] for i in range(n)]
src,out=sys.argv[1],sys.argv[2]
for f in sorted(os.listdir(src)):
    if not f.endswith(".html"): continue
    p=P(); p.feed(open(os.path.join(src,f),encoding="utf-8",errors="replace").read())
    j=json.load(open(os.path.join(out,f[:-5]+".xlsx.json")))
    if not p.sheets and p.loose:
        p.sheets=[[h[1],g] for h,g in zip(j["headings"],p.loose)]
    xlsx=os.path.join(os.path.dirname(os.path.normpath(src)).replace("lo-en",""), os.path.basename(os.path.normpath(src)), "xlsx", f[:-5]+".xlsx")
    dims=xml_dims(xlsx) if os.path.exists(xlsx) else {}
    tables=[]
    for name,g in p.sheets:
        filled=[(r,c) for (r,c),v in g.items() if v]
        if not filled: continue
        r0=min(r for r,_ in filled); r1=max(r for r,_ in filled); c0=min(c for _,c in filled); c1=max(c for _,c in filled)
        cells=[g[k] for k in sorted(filled) if len(g[k])<=60]
        num=[c for c in cells if re.search(r"\d",c) and re.search(r"[%,./:$£€-]",c)]
        pick=[]
        for c in cells[:1]+num[len(num)//2:len(num)//2+1]+cells[len(cells)//2:len(cells)//2+1]+cells[-1:]:
            if c not in pick: pick.append(c)
        rows,cols=dims.get(name,(r1-r0+1,c1-c0+1))
        tables.append({"sheet":name,"rows":rows,"cols":cols,"cells":pick})
    j["headings"]=[[0,n] for n,_ in p.sheets][:30]
    j["tables"]=spread(tables,10); j["order"]=[n for n,_ in spread(p.sheets,8)]
    json.dump(j,open(os.path.join(out,f[:-5]+".xlsx.json"),"w"),ensure_ascii=False,indent=1)
    print(f, len(p.sheets), "表", [ (t["rows"],t["cols"]) for t in tables][:6])

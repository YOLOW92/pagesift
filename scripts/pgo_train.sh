#!/usr/bin/env bash
# PGO 训练负载：用插桩版 pagesift 跑一遍常见的本地转换，让它写出 .profraw。
#   scripts/pgo_train.sh <pagesift 二进制> [工作目录]
# 全程离线：输入只来自仓库里已有的文件（各处的 Markdown 文档、CSV、PDF）和这里现场生成的
# 网页与 Office 文档，不下载任何东西，也不往仓库里加大文件。
# 覆盖的路径：
#   1. convert：HTML 转 Markdown（只取正文、--full、--base-url、stdin、GBK 编码）
#   2. 本机 127.0.0.1 上起一个静态服务器：crawl 整站，scrape 单页和 PDF/DOCX/XLSX/CSV
# 第 1 部分任何一条失败都直接退出非零（说明二进制本身有问题）；第 2 部分依赖本机网络，
# 失败只打警告，已经跑过的部分照样留下画像。release.yml 里 PGO 任何一步失败都会回退到普通编译。
# macOS 自带 bash 3.2，这里不用关联数组、mapfile 之类的新语法。
set -euo pipefail

if [ $# -lt 1 ]; then
    echo "usage: $0 <pagesift-binary> [work-dir]" >&2
    exit 2
fi
BIN=$1
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${2:-$(mktemp -d "${TMPDIR:-/tmp}/pgo-train.XXXXXX")}
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
case $BIN in /*) ;; *) BIN=$(pwd)/$BIN ;; esac

# 不读用户配置、不走代理：训练只连 127.0.0.1
export PAGESIFT_CONFIG=
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost

START=$(date +%s)
"$BIN" --version

# ---- 生成语料 ----
# site/：几十个带导航、侧栏、页脚、表格、列表、代码块的页面，正文取自仓库里的 Markdown 文档，
# 页面之间互相链接，供 crawl 用；docs/：现场拼出的 PDF/DOCX/XLSX/CSV 和仓库里已有的 PDF/CSV。
python3 -I - "$ROOT" "$WORK" <<'PY'
import html, os, random, re, sys, zipfile, zlib

root, work = sys.argv[1], sys.argv[2]
site = os.path.join(work, "site")
docs = os.path.join(site, "docs")
os.makedirs(docs, exist_ok=True)
rnd = random.Random(20261029)

# 仓库里的 Markdown 当正文素材（跳过第三方目录和过大的结果文件）
sources = []
for base in ("docs", "crates", "benchmarks", "."):
    top = os.path.join(root, base)
    for dirpath, dirnames, filenames in os.walk(top):
        dirnames[:] = [d for d in dirnames if d not in ("target", "node_modules", ".git", "vendor", ".claude")]
        if base == "." and dirpath != top:
            dirnames[:] = []
        for name in sorted(filenames):
            path = os.path.join(dirpath, name)
            if name.endswith(".md") and path not in sources and os.path.getsize(path) < 200_000:
                sources.append(path)
sources = sorted(set(sources))[:120]

def inline(text):
    text = html.escape(text, quote=False)
    text = re.sub(r"`([^`]+)`", r"<code>\1</code>", text)
    text = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", text)
    text = re.sub(r"\[([^\]]+)\]\(([^)\s]+)\)", r'<a href="\2">\1</a>', text)
    return text

def md_to_html(md):
    out, para, in_code, in_list, table = [], [], False, False, []
    def flush():
        nonlocal para, in_list, table
        if para:
            out.append("<p>" + inline(" ".join(para)) + "</p>")
            para = []
        if in_list:
            out.append("</ul>")
            in_list = False
        if table:
            rows = [r for r in table if not re.match(r"^\|[\s:|-]+\|$", r)]
            cells = [[c.strip() for c in r.strip("|").split("|")] for r in rows]
            if cells:
                out.append("<table><thead><tr>" + "".join("<th>%s</th>" % inline(c) for c in cells[0]) + "</tr></thead><tbody>")
                for r in cells[1:]:
                    out.append("<tr>" + "".join("<td>%s</td>" % inline(c) for c in r) + "</tr>")
                out.append("</tbody></table>")
            table = []
    for line in md.splitlines():
        if line.startswith("```"):
            flush()
            out.append("</code></pre>" if in_code else "<pre><code>")
            in_code = not in_code
            continue
        if in_code:
            out.append(html.escape(line))
            continue
        m = re.match(r"^(#{1,6})\s+(.*)", line)
        if m:
            flush()
            level = min(len(m.group(1)) + 1, 6)
            out.append("<h%d>%s</h%d>" % (level, inline(m.group(2)), level))
        elif line.startswith("|"):
            if para or in_list:
                flush()
            table.append(line)
        elif re.match(r"^\s*([-*]|\d+\.)\s+", line):
            if para or table:
                flush()
            if not in_list:
                out.append("<ul>")
                in_list = True
            out.append("<li>" + inline(re.sub(r"^\s*([-*]|\d+\.)\s+", "", line)) + "</li>")
        elif line.startswith(">"):
            flush()
            out.append("<blockquote><p>" + inline(line.lstrip("> ")) + "</p></blockquote>")
        elif not line.strip():
            flush()
        else:
            if in_list or table:
                flush()
            para.append(line.strip())
    flush()
    if in_code:
        out.append("</code></pre>")
    return "\n".join(out)

WORDS = ("data pipeline crawler markdown extraction latency throughput cache policy robots sitemap "
         "render browser fetch parser table column record archive index search query result "
         "数据 抓取 页面 正文 表格 链接 缓存 并发 性能 文档").split()

def sentence(n):
    return " ".join(rnd.choice(WORDS) for _ in range(n)).capitalize() + "."

def synthetic_body(i):
    kind = i % 4
    parts = ["<h2>%s</h2>" % sentence(4)]
    if kind == 0:   # 长文章
        for _ in range(12):
            parts.append("<p>%s <a href=\"page%d.html\">%s</a> %s</p>" % (sentence(25), (i + 3) % 40, sentence(3), sentence(18)))
        parts.append("<blockquote><p>%s</p></blockquote>" % sentence(20))
        parts.append('<figure><img src="/img/%d.png" alt="%s"><figcaption>%s</figcaption></figure>' % (i, sentence(4), sentence(6)))
    elif kind == 1:  # 数据表
        parts.append("<table><caption>%s</caption><thead><tr>%s</tr></thead><tbody>" % (sentence(3), "".join("<th>c%d</th>" % c for c in range(8))))
        for r in range(60):
            parts.append("<tr>" + "".join("<td>%s</td>" % (rnd.randint(0, 99999) if c else sentence(2)) for c in range(8)) + "</tr>")
        parts.append("</tbody></table>")
    elif kind == 2:  # 嵌套列表与定义列表
        for _ in range(6):
            parts.append("<ul>" + "".join("<li>%s<ol>%s</ol></li>" % (sentence(8), "".join("<li>%s</li>" % sentence(5) for _ in range(3))) for _ in range(5)) + "</ul>")
        parts.append("<dl>" + "".join("<dt>%s</dt><dd>%s</dd>" % (sentence(2), sentence(12)) for _ in range(8)) + "</dl>")
    else:            # 论坛/评论
        for c in range(15):
            parts.append('<article class="comment"><header><span class="author">user%d</span> <time datetime="2026-10-%02d">%d days ago</time></header><p>%s</p><p>%s &amp; %s &lt;ok&gt;</p></article>' % (c, c % 28 + 1, c, sentence(30), sentence(10), sentence(4)))
    return "\n".join(parts)

def page(title, body, n_pages):
    nav = "".join('<li><a href="/page%d.html">Section %d</a></li>' % (k, k) for k in range(0, n_pages, 3))
    side = "".join('<li><a href="/doc%d.html">%s</a></li>' % (k, sentence(3)) for k in range(0, 30, 2))
    return """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>{t}</title>
<meta name="description" content="{d}"><meta property="og:title" content="{t}">
<link rel="canonical" href="/x"><style>body{{font:14px sans-serif}} .ad{{display:block}}</style>
<script>window.dataLayer=[];function track(){{return 1}}</script>
<script type="application/ld+json">{{"@context":"https://schema.org","@type":"Article","headline":"{t}"}}</script>
</head><body>
<div class="cookie-banner" role="dialog">We use cookies. <button>Accept</button></div>
<header class="site-header"><a href="/" class="logo">Example</a><nav><ul>{nav}</ul></nav>
<form action="/search"><input name="q" placeholder="Search"></form></header>
<div class="layout"><aside class="sidebar"><h3>Related</h3><ul>{side}</ul><div class="ad">Sponsored</div></aside>
<main><article><h1>{t}</h1><p class="byline">By Jane Doe · <time datetime="2026-10-01">Oct 1, 2026</time></p>
{b}
</article></main></div>
<footer><p>&copy; 2026 Example Inc. All rights reserved.</p><ul><li><a href="/privacy">Privacy</a></li><li><a href="/terms">Terms</a></li></ul>
<p><a href="/docs/report.pdf">Report (PDF)</a> <a href="/docs/report.docx">DOCX</a> <a href="/docs/data.xlsx">XLSX</a> <a href="/docs/data.csv">CSV</a></p></footer>
</body></html>
""".format(t=html.escape(title), d=html.escape(sentence(12)), nav=nav, side=side, b=body)

N = 40
for i in range(N):
    with open(os.path.join(site, "page%d.html" % i), "w", encoding="utf-8") as f:
        f.write(page("Page %d: %s" % (i, sentence(4)), synthetic_body(i), N))
for i, src in enumerate(sources):
    with open(src, encoding="utf-8", errors="replace") as f:
        md = f.read()
    title = os.path.relpath(src, root)
    with open(os.path.join(site, "doc%d.html" % i), "w", encoding="utf-8") as f:
        f.write(page(title, md_to_html(md), N))
links = "".join('<li><a href="page%d.html">Page %d</a></li>' % (i, i) for i in range(N))
links += "".join('<li><a href="doc%d.html">Doc %d</a></li>' % (i, i) for i in range(len(sources)))
with open(os.path.join(site, "index.html"), "w", encoding="utf-8") as f:
    f.write(page("Home", "<ul>%s</ul>" % links, N))
# GBK 编码的中文页，走字符集探测
with open(os.path.join(site, "gbk.html"), "wb") as f:
    body = "".join("<p>%s</p>" % ("这是一个用于字符集探测的中文段落，包含标点、数字 %d 和英文 word。" % k * 4) for k in range(30))
    f.write(('<html><head><meta charset="gbk"><title>中文页面</title></head><body><main><h1>标题</h1>%s</main></body></html>' % body).encode("gbk"))

# ---- 文档 ----
rows = [["id", "name", "category", "price", "qty", "note"]] + [
    [str(r), sentence(2), rnd.choice(["a", "b", "c"]), "%.2f" % (rnd.random() * 1000), str(rnd.randint(1, 500)), sentence(5)] for r in range(400)]
with open(os.path.join(docs, "data.csv"), "w", encoding="utf-8") as f:
    for r in rows:
        f.write(",".join('"%s"' % c if "," in c or " " in c else c for c in r) + "\n")

def xml_escape(s):
    return html.escape(s, quote=False)

def write_zip(path, files):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in files:
            z.writestr(name, data)

RELS = '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="%s"/></Relationships>'
W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
paras = []
for k in range(80):
    if k % 10 == 0:
        paras.append('<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>%s</w:t></w:r></w:p>' % xml_escape(sentence(4)))
    paras.append('<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">%s </w:t></w:r><w:r><w:t>%s</w:t></w:r></w:p>' % (xml_escape(sentence(3)), xml_escape(sentence(30))))
tbl = "<w:tbl>" + "".join("<w:tr>" + "".join("<w:tc><w:p><w:r><w:t>%s</w:t></w:r></w:p></w:tc>" % xml_escape(c) for c in r) + "</w:tr>" for r in rows[:30]) + "</w:tbl>"
write_zip(os.path.join(docs, "report.docx"), [
    ("[Content_Types].xml", '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'),
    ("_rels/.rels", RELS % "word/document.xml"),
    ("word/document.xml", '<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="%s"><w:body>%s%s</w:body></w:document>' % (W, "".join(paras), tbl)),
])

S = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
def cell_ref(c, r):
    return "%s%d" % (chr(ord("A") + c), r + 1)
sheet_rows = []
for r, row in enumerate(rows):
    cells = []
    for c, v in enumerate(row):
        if r and c in (0, 3, 4):
            cells.append('<c r="%s"><v>%s</v></c>' % (cell_ref(c, r), v))
        else:
            cells.append('<c r="%s" t="inlineStr"><is><t>%s</t></is></c>' % (cell_ref(c, r), xml_escape(v)))
    sheet_rows.append('<row r="%d">%s</row>' % (r + 1, "".join(cells)))
write_zip(os.path.join(docs, "data.xlsx"), [
    ("[Content_Types].xml", '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>'),
    ("_rels/.rels", RELS % "xl/workbook.xml"),
    ("xl/workbook.xml", '<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="%s" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>' % S),
    ("xl/_rels/workbook.xml.rels", '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>'),
    ("xl/worksheets/sheet1.xml", '<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="%s"><sheetData>%s</sheetData></worksheet>' % (S, "".join(sheet_rows))),
])

# 多页文字 PDF：内容流压缩，交叉引用表按实际偏移写
def pdf_text(n):
    # Type1 标准字体只有拉丁字符，PDF 里不放中文词
    s = " ".join(w for w in sentence(n).split() if w.isascii())
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
objs = {1: b"<< /Type /Catalog /Pages 2 0 R >>", 3: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"}
kids, nxt = [], 4
for p in range(12):
    lines = ["BT /F1 16 Tf 72 740 Td (Chapter %d: %s) Tj ET" % (p + 1, pdf_text(6))]
    y = 710
    for _ in range(40):
        lines.append("BT /F1 10 Tf 72 %d Td (%s) Tj ET" % (y, pdf_text(16)))
        y -= 15
    stream = zlib.compress("\n".join(lines).encode("latin-1", "replace"))
    page_id, content_id = nxt, nxt + 1
    nxt += 2
    objs[content_id] = b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(stream) + stream + b"\nendstream"
    objs[page_id] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents %d 0 R >>" % content_id
    kids.append(page_id)
objs[2] = b"<< /Type /Pages /Kids [%s] /Count %d >>" % (" ".join("%d 0 R" % k for k in kids).encode(), len(kids))
out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
offsets = {}
for oid in sorted(objs):
    offsets[oid] = len(out)
    out += b"%d 0 obj\n" % oid + objs[oid] + b"\nendobj\n"
xref = len(out)
out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
for oid in sorted(objs):
    out += b"%010d 00000 n \n" % offsets[oid]
out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
with open(os.path.join(docs, "report.pdf"), "wb") as f:
    f.write(out)
print("corpus: %d synthetic pages, %d pages from repo Markdown" % (N, len(sources)))
PY

# 仓库里已有的 PDF/CSV 也放进去（最多 20 个；用 sed 截断而不是 head，免得 find 收到 SIGPIPE 被 pipefail 判失败）
find "$ROOT/benchmarks" -type f \( -name '*.pdf' -o -name '*.csv' \) -size -2M 2>/dev/null | sort | sed -n '1,20p' |
while IFS= read -r f; do
    cp "$f" "$WORK/site/docs/repo-$(basename "$f")"
done

# ---- 1. convert（必须成功） ----
OUT=$WORK/out
mkdir -p "$OUT"
n=0
for f in "$WORK"/site/*.html; do
    name=$(basename "$f" .html)
    "$BIN" convert "$f" > "$OUT/$name.md"
    "$BIN" convert --full --base-url "https://example.com/$name.html" "$f" > "$OUT/$name.full.md"
    n=$((n + 1))
done
# stdin 和指定字符集
"$BIN" convert < "$WORK/site/page0.html" > "$OUT/stdin.md"
"$BIN" convert --charset GBK "$WORK/site/gbk.html" > "$OUT/gbk-explicit.md"
[ -s "$OUT/page1.md" ] || { echo "convert produced empty output" >&2; exit 1; }
echo "convert: $n files x 2 modes done ($(( $(date +%s) - START ))s)"

# ---- 2. 本机静态服务器上的 crawl / scrape（尽力而为） ----
# 带超时执行：macOS 没有 timeout 命令，用后台进程加看门狗
run_limited() {
    local limit=$1
    shift
    "$@" &
    local pid=$!
    ( sleep "$limit"; kill "$pid" 2>/dev/null ) &
    local dog=$!
    local rc=0
    wait "$pid" || rc=$?
    kill "$dog" 2>/dev/null || true
    wait "$dog" 2>/dev/null || true
    return $rc
}

PORT_FILE=$WORK/port
rm -f "$PORT_FILE"
python3 -I - "$WORK/site" "$PORT_FILE" <<'PY' > "$WORK/server.log" 2>&1 &
import functools, http.server, os, sys
root, port_file = sys.argv[1], sys.argv[2]
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=root)
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
with open(port_file + ".tmp", "w") as f:
    f.write(str(server.server_address[1]))
os.rename(port_file + ".tmp", port_file)
server.serve_forever()
PY
SERVER=$!
trap 'kill "$SERVER" 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do
    [ -s "$PORT_FILE" ] && break
    sleep 0.1
done

warn() { echo "::warning::pgo_train: $*"; }
if [ ! -s "$PORT_FILE" ]; then
    warn "local HTTP server did not start; skipping crawl/scrape training"
else
    BASE=http://127.0.0.1:$(cat "$PORT_FILE")
    # 本机服务器不用礼貌间隔（默认同一主机每秒一次，crawl 会被拖到几分钟）；缓存放在工作目录里
    cat > "$WORK/pagesift.toml" <<EOF
[policy]
interval_ms = 0
per_host_concurrency = 8

[cache]
enabled = true
dir = "$WORK/cache"
EOF
    export PAGESIFT_CONFIG=$WORK/pagesift.toml
    NET="--engine http --allow-cidr 127.0.0.0/8"
    # shellcheck disable=SC2086  # NET 需要按空格拆成多个参数
    run_limited 120 "$BIN" crawl "$BASE/index.html" --limit 400 --scope domain --ignore-robots \
        $NET --out "$OUT/crawl" > "$OUT/crawl.log" 2>&1 ||
        warn "crawl failed (see $OUT/crawl.log)"
    for path in page5.html doc3.html gbk.html docs/report.pdf docs/report.docx docs/data.xlsx docs/data.csv; do
        # shellcheck disable=SC2086
        run_limited 60 "$BIN" scrape "$BASE/$path" --ignore-robots $NET > "$OUT/scrape-$(basename "$path").md" 2>>"$OUT/scrape.log" ||
            warn "scrape $path failed"
    done
    for f in "$WORK"/site/docs/repo-*; do
        [ -e "$f" ] || continue
        # shellcheck disable=SC2086
        run_limited 60 "$BIN" scrape "$BASE/docs/$(basename "$f")" --ignore-robots $NET --format json \
            > "$OUT/scrape-$(basename "$f").json" 2>>"$OUT/scrape.log" || warn "scrape $(basename "$f") failed"
    done
    crawled=$(find "$OUT/crawl" -name '*.md' 2>/dev/null | wc -l | tr -d ' ')
    echo "crawl: $crawled pages written; scrape: documents done ($(( $(date +%s) - START ))s)"
fi

echo "pgo_train: finished in $(( $(date +%s) - START ))s, outputs in $OUT"

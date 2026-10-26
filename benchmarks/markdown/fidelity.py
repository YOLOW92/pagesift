"""HTML → Markdown 保真度：随机往返测试。

用固定种子随机生成文档结构（标题、段落、强调、删除线、行内代码、链接、图片、换行、多层列表、表格、
代码块、引用、分隔线、公式，以及必须原样保留的 Markdown 特殊字符），渲染成接近真实网站输出的 HTML
（外层 div/span 包装、源码缩进和换行、实体、语法高亮的代码、KaTeX/MathJax 的公式结构）。
各工具把 HTML 转成 Markdown，再用同一个 GFM 解析器（markdown-it-py，gfm-like）解析回结构，逐类和原文档比对。

正文里成对的反引号不算"必须原样保留的文字"：网页正文里出现的成对反引号几乎都是作者写的 Markdown 没被渲染，
原意就是行内代码，转成代码和保留成字面反引号都算合理，所以生成器不出这种情况。

用法（需要 markdown-it-py 的 Python，比如 Crawl4AI 的虚拟环境）：
  python -I fidelity.py gen <目录> [文档数] [种子]     生成 <目录>/NNN.html 和 <目录>/gold.json
  python -I fidelity.py score <目录> <工具名>=<输出目录> ...   输出目录里是同名的 NNN.md

每类按多重集合算精确率、召回率、F1（都是越高越好）；总分是各类 F1 的平均。
"""
import collections, html, json, os, random, re, sys

# ---------------------------------------------------------------------------
# 随机文档

WORDS = ("alpha beta gamma delta river stone light market garden engine signal paper window "
         "planet music forest number letter yellow simple quiet rapid modern ancient yearly data "
         "城市 数据 模型 東京 データ 서울 Müller façade naïve").split()
# 必须当作普通文字保留的 Markdown 特殊写法
SPECIAL = ["*not bold*", "_not em_", "2 * 3 = 6", "a_b_c", "# not a heading", "1. not a list",
           "- not a list", "[not a link]", "<tag>", "a & b", "x | y", "50%", "C:\\path",
           "~not strike~", "> not quote", "+1", "!important"]
LANGS = ["python", "javascript", "rust", "bash", "json", "go", None]


class Gen:
    def __init__(self, seed):
        self.r = random.Random(seed)

    def words(self, lo=2, hi=6):
        return " ".join(self.r.choice(WORDS) for _ in range(self.r.randint(lo, hi)))

    def plain(self, allow_special=True):
        s = self.words()
        if allow_special and self.r.random() < 0.3:
            s += " " + self.r.choice(SPECIAL) + " " + self.words(1, 3)
        return s

    def inline(self, depth=0, allow_special=True):
        out = [("text", self.plain(allow_special) + " ")]
        for _ in range(self.r.randint(1, 4)):
            k = self.r.random()
            if k < 0.15 and depth == 0:
                out.append(("strong", [("text", self.words(1, 3))]))
            elif k < 0.28 and depth == 0:
                out.append(("em", [("text", self.words(1, 3))]))
            elif k < 0.34 and depth == 0:
                out.append(("del", [("text", self.words(1, 2))]))
            elif k < 0.46:
                out.append(("code", self.r.choice(["x = 1", "len(s)", "a*b", "foo_bar", "<div>", "a|b", "cfg.get('k')"])))
            elif k < 0.6 and depth == 0:
                href = self.r.choice(["https://example.com/a", "https://example.com/p?q=1&r=2",
                                      "https://example.com/docs/page_(1)", "/relative/path", "https://例子.cn/页面"])
                kids = [("text", self.words(1, 3))]
                if self.r.random() < 0.2:
                    kids = [("strong", kids)]
                out.append(("link", href, kids))
            elif k < 0.66 and depth == 0:
                out.append(("img", f"https://example.com/img/{self.r.randint(1, 99)}.png", self.words(1, 3)))
            elif k < 0.7 and depth == 0:
                out.append(("br",))
            elif k < 0.75 and depth == 0:
                out.append(("math", self.r.choice(["x^2 + y^2 = z^2", r"\frac{a}{b}", r"\sum_{i=1}^{n} i", r"\alpha + \beta"])))
            out.append(("text", " " + self.words(1, 4) + " "))
        return out

    def list_block(self, depth=0):
        ordered = self.r.random() < 0.4
        items = []
        for _ in range(self.r.randint(2, 4)):
            sub = self.list_block(depth + 1) if depth < 2 and self.r.random() < 0.3 else None
            items.append((self.inline(1), sub))
        return ("list", ordered, items)

    def code_text(self, lang):
        lines = {
            "python": ["def f(x):", "    return x * 2  # double", "", "if a < b and c > d:", "    print(\"<&>\")"],
            "javascript": ["const a = `t${x}`;", "if (a && b) { return a < b; }", "  // comment *x*"],
            "rust": ["fn main() {", "    let v: Vec<u8> = vec![1, 2];", "    println!(\"{:?}\", v);", "}"],
            "bash": ["for f in *.md; do", "  echo \"$f\" | grep -v '_x_'", "done"],
            "json": ["{", "  \"a\": [1, 2],", "  \"b\": \"x|y\"", "}"],
            "go": ["func main() {", "\tfmt.Println(\"hi\")", "}"],
            None: ["plain  text  block", "    indented line", "*stars* and _under_"],
        }[lang]
        n = self.r.randint(2, len(lines))
        start = self.r.randint(0, len(lines) - n)
        return "\n".join(lines[start:start + n])

    def doc(self):
        blocks = [("h", 1, [("text", self.words(2, 5))])]
        for _ in range(self.r.randint(8, 16)):
            k = self.r.random()
            if k < 0.12:
                blocks.append(("h", self.r.randint(2, 6), [("text", self.words(2, 5))]))
            elif k < 0.42:
                blocks.append(("p", self.inline()))
            elif k < 0.55:
                blocks.append(self.list_block())
            elif k < 0.65:
                cols = self.r.randint(2, 4)
                header = [[("text", self.words(1, 2))] for _ in range(cols)]
                rows = []
                for _ in range(self.r.randint(2, 4)):
                    row = []
                    for _ in range(cols):
                        c = self.r.random()
                        if c < 0.15:
                            row.append([("code", "a|b" if self.r.random() < 0.3 else "v1")])
                        elif c < 0.25:
                            row.append([("link", "https://example.com/t", [("text", self.words(1, 2))])])
                        elif c < 0.32:
                            row.append([("text", "x | y")])
                        else:
                            row.append([("text", self.words(1, 3))])
                    rows.append(row)
                blocks.append(("table", header, rows, self.r.random() < 0.8))
            elif k < 0.76:
                lang = self.r.choice(LANGS)
                blocks.append(("code", lang, self.code_text(lang), self.r.random() < 0.35))
            elif k < 0.84:
                inner = [("p", self.inline())]
                if self.r.random() < 0.3:
                    inner.append(self.list_block(1))
                blocks.append(("quote", inner))
            elif k < 0.89:
                blocks.append(("hr",))
            elif k < 0.94:
                blocks.append(("mathblock", self.r.choice([r"E = mc^2", r"\int_0^1 x\,dx = \frac{1}{2}", r"a^2 + b^2 = c^2"])))
            else:
                blocks.append(("p", self.inline()))
        return blocks


# ---------------------------------------------------------------------------
# 渲染成 HTML（模仿真实网站：外层包装、缩进、实体、高亮代码、KaTeX）

def esc(s):
    return html.escape(s, quote=False)


def katex(tex, display):
    inner = (f'<span class="katex"><span class="katex-mathml"><math xmlns="http://www.w3.org/1998/Math/MathML"'
             f'{" display=\"block\"" if display else ""}><semantics><mrow><mi>x</mi></mrow>'
             f'<annotation encoding="application/x-tex">{esc(tex)}</annotation></semantics></math></span>'
             f'<span class="katex-html" aria-hidden="true"><span class="base">{esc(tex)}</span></span></span>')
    return f'<span class="katex-display">{inner}</span>' if display else inner


class Render:
    def __init__(self, seed):
        self.r = random.Random(seed ^ 0x5EED)

    def inline(self, nodes):
        out = []
        for n in nodes:
            k = n[0]
            if k == "text":
                t = esc(n[1])
                if self.r.random() < 0.2:
                    t = f"<span>{t}</span>"
                out.append(t)
            elif k == "strong":
                tag = self.r.choice(["strong", "b"])
                out.append(f"<{tag}>{self.inline(n[1])}</{tag}>")
            elif k == "em":
                tag = self.r.choice(["em", "i"])
                out.append(f"<{tag}>{self.inline(n[1])}</{tag}>")
            elif k == "del":
                tag = self.r.choice(["del", "s"])
                out.append(f"<{tag}>{self.inline(n[1])}</{tag}>")
            elif k == "code":
                out.append(f"<code>{esc(n[1])}</code>")
            elif k == "link":
                title = ' title="More info"' if self.r.random() < 0.2 else ""
                out.append(f'<a href="{html.escape(n[1])}"{title}>{self.inline(n[2])}</a>')
            elif k == "img":
                out.append(f'<img src="{html.escape(n[1])}" alt="{html.escape(n[2])}" width="300">')
            elif k == "br":
                out.append("<br>\n")
            elif k == "math":
                x = self.r.random()
                if x < 0.35:
                    out.append(katex(n[1], False))
                elif x < 0.7:
                    out.append(f'<span class="MathJax_Preview"></span><script type="math/tex">{n[1]}</script>')
                else:
                    # 未渲染的 MathJax 页面（HTTP 直接抓到的源码）：TeX 原样写在正文里
                    out.append(esc(f"\\({n[1]}\\)"))
        return "".join(out)

    def lst(self, b, indent):
        _, ordered, items = b
        tag = "ol" if ordered else "ul"
        pad = "  " * indent
        loose = self.r.random() < 0.3
        s = f"{pad}<{tag}>\n"
        for text, sub in items:
            body = self.inline(text)
            if loose:
                body = f"<p>{body}</p>"
            s += f"{pad}  <li>{body}"
            if sub:
                s += "\n" + self.lst(sub, indent + 2) + pad + "  "
            s += "</li>\n"
        return s + f"{pad}</{tag}>\n"

    def block(self, b):
        k = b[0]
        if k == "h":
            return f"<h{b[1]}>{self.inline(b[2])}</h{b[1]}>\n"
        if k == "p":
            return f"<p>\n  {self.inline(b[1])}\n</p>\n"
        if k == "list":
            return self.lst(b, 0)
        if k == "table":
            _, header, rows, thead = b
            cell = lambda tag, c: f"<{tag}>{self.inline(c)}</{tag}>"
            h = "<tr>" + "".join(cell("th", c) for c in header) + "</tr>\n"
            body = "".join("<tr>" + "".join(cell("td", c) for c in r) + "</tr>\n" for r in rows)
            if thead:
                return f'<div class="table-wrap"><table class="data">\n<thead>{h}</thead>\n<tbody>\n{body}</tbody>\n</table></div>\n'
            return f"<table>\n<tbody>\n{h}{body}</tbody>\n</table>\n"
        if k == "code":
            _, lang, text, highlighted = b
            if highlighted:
                # 语法高亮：每个词包一层 span（Pygments/highlight.js 的样子）
                parts = re.split(r"(\s+)", text)
                inner = "".join(p if p.isspace() or not p else f'<span class="tok">{esc(p)}</span>' for p in parts)
                cls = f' class="language-{lang}"' if lang else ""
                return f'<div class="highlight"><pre><code{cls}>{inner}</code></pre></div>\n'
            cls = f' class="language-{lang}"' if lang else ""
            return f"<pre><code{cls}>{esc(text)}</code></pre>\n"
        if k == "quote":
            return "<blockquote>\n" + "".join(self.block(x) for x in b[1]) + "</blockquote>\n"
        if k == "hr":
            return "<hr>\n"
        if k == "mathblock":
            x = self.r.random()
            if x < 0.35:
                return f"<p>{katex(b[1], True)}</p>\n"
            if x < 0.7:
                return f'<script type="math/tex; mode=display">{b[1]}</script>\n'
            return f"<p>{esc('$$' + b[1] + '$$')}</p>\n"

        raise ValueError(k)

    def page(self, blocks):
        body = ""
        for b in blocks:
            s = self.block(b)
            if self.r.random() < 0.3:
                s = f'<div class="block">\n{s}</div>\n'
            body += s
        return (f"<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Doc</title>"
                f"<script src=\"https://cdn.jsdelivr.net/npm/mathjax@3/es5/tex-mml-chtml.js\"></script></head>"
                f"<body><main><article>\n{body}</article></main></body></html>\n")


# ---------------------------------------------------------------------------
# 标准答案：直接从结构写出的 Markdown（校准评分器用，各类应接近 1.0）

def md_escape(t, table=False):
    t = re.sub(r"([\\`*_\[\]<>~#!])", r"\\\1", t)
    t = t.replace("&", "&amp;")
    if table:
        t = t.replace("|", "\\|")
    return re.sub(r"^(\s*)(\d+)\.", r"\1\2\\.", re.sub(r"^(\s*)([-+])", r"\1\\\2", t))


def md_inline(nodes, table=False):
    out = []
    for n in nodes:
        k = n[0]
        if k == "text":
            out.append(md_escape(n[1], table))
        elif k == "strong":
            out.append("**" + md_inline(n[1], table).strip() + "**")
        elif k == "em":
            out.append("*" + md_inline(n[1], table).strip() + "*")
        elif k == "del":
            out.append("~~" + md_inline(n[1], table).strip() + "~~")
        elif k == "code":
            c = n[1].replace("|", "\\|") if table else n[1]
            out.append("`" + c + "`")
        elif k == "link":
            out.append("[" + md_inline(n[2], table).strip() + "](<" + n[1] + ">)")
        elif k == "img":
            out.append("![" + md_escape(n[2]) + "](" + n[1] + ")")
        elif k == "br":
            out.append("\\\n")
        elif k == "math":
            out.append("$" + n[1] + "$")
    return "".join(out)


def md_block(b, prefix=""):
    k = b[0]
    if k == "h":
        return "#" * b[1] + " " + md_inline(b[2]).strip() + "\n\n"
    if k == "p":
        return md_inline(b[1]).strip() + "\n\n"
    if k == "list":
        return md_list(b, "") + "\n"
    if k == "table":
        row = lambda cells: "| " + " | ".join(md_inline(c, True).strip() for c in cells) + " |\n"
        return row(b[1]) + "|" + " --- |" * len(b[1]) + "\n" + "".join(row(r) for r in b[2]) + "\n"
    if k == "code":
        return "```" + (b[1] or "") + "\n" + b[2] + "\n```\n\n"
    if k == "quote":
        inner = "".join(md_block(x) for x in b[1]).rstrip("\n")
        return "\n".join("> " + line if line else ">" for line in inner.split("\n")) + "\n\n"
    if k == "hr":
        return "---\n\n"
    if k == "mathblock":
        return "$$" + b[1] + "$$\n\n"
    raise ValueError(k)


def md_list(b, indent):
    out = ""
    for i, (text, sub) in enumerate(b[2], 1):
        marker = f"{i}. " if b[1] else "- "
        out += indent + marker + md_inline(text).strip() + "\n"
        if sub:
            out += md_list(sub, indent + " " * len(marker))
    return out


# ---------------------------------------------------------------------------
# 结构特征

def norm(s):
    return " ".join(s.split())


def plain(nodes):
    out = []
    for n in nodes:
        if n[0] == "text":
            out.append(n[1])
        elif n[0] in ("strong", "em", "del"):
            out.append(plain(n[1]))
        elif n[0] == "code":
            out.append(n[1])
        elif n[0] == "link":
            out.append(plain(n[2]))
    return norm("".join(out))


def gold_features(blocks):
    f = collections.defaultdict(list)

    def inline(nodes):
        for n in nodes:
            if n[0] == "text":
                f["text"].extend(n[1].split())
            elif n[0] in ("strong", "em", "del"):
                f[{"strong": "strong", "em": "em", "del": "strike"}[n[0]]].append(plain(n[1]))
                inline(n[1])
            elif n[0] == "code":
                f["code_inline"].append(n[1])
                f["text"].extend(n[1].split())
            elif n[0] == "link":
                f["link"].append((plain(n[2]), n[1]))
                inline(n[2])
            elif n[0] == "img":
                f["image"].append((n[2], n[1]))
            elif n[0] == "br":
                f["break"].append(1)
            elif n[0] == "math":
                f["math"].append(norm(n[1]).replace(" ", ""))

    def lst(b, depth):
        for text, sub in b[2]:
            f["list_item"].append((depth, b[1], plain(text)))
            inline(text)
            if sub:
                lst(sub, depth + 1)

    def block(b, quote=0):
        k = b[0]
        if k == "h":
            f["heading"].append((b[1], plain(b[2])))
            inline(b[2])
        elif k == "p":
            if quote:
                f["quote"].append((quote, plain(b[1])))
            inline(b[1])
        elif k == "list":
            lst(b, 0)
        elif k == "table":
            for ci, c in enumerate(b[1]):
                f["table"].append((0, ci, plain(c)))
                inline(c)
            for ri, r in enumerate(b[2], 1):
                for ci, c in enumerate(r):
                    f["table"].append((ri, ci, plain(c)))
                    inline(c)
        elif k == "code":
            f["code_block"].append((b[1] or "", b[2].rstrip("\n")))
        elif k == "quote":
            for x in b[1]:
                block(x, quote + 1)
        elif k == "hr":
            f["hr"].append(1)
        elif k == "mathblock":
            f["math"].append(norm(b[1]).replace(" ", ""))

    for b in blocks:
        block(b)
    return {k: v for k, v in f.items()}


def md_features(md):
    from markdown_it import MarkdownIt
    f0 = collections.defaultdict(list)
    # 公式先取出来单独计分，再从文本里去掉：$…$、$$…$$、\(…\)、\[…\]（后两种要像 TeX，免得把转义的方括号当成公式）
    def take(m):
        tex = next(g for g in m.groups() if g is not None)
        if (m.group(0).startswith("\\") and not re.search(r"[\\^_{}=]", tex)):
            return m.group(0)
        f0["math"].append(norm(tex).replace(" ", ""))
        return " "
    md = re.sub(r"\$\$(.+?)\$\$|\\\[(.+?)\\\]|(?<![\\$])\$([^$\n]+?)\$|\\\((.+?)\\\)", take, md, flags=re.S)
    p = MarkdownIt("gfm-like").disable("linkify")
    toks = p.parse(md)
    f = f0
    stack = []  # 当前所在的容器
    table = -1
    row = -1
    col = -1
    pending_heading = None

    def inline_feats(children):
        textbuf = []
        open_spans = []
        link_stack = []
        for t in children:
            ty = t.type
            if ty == "text":
                textbuf.append(t.content)
                for s in open_spans:
                    s[1].append(t.content)
                for l in link_stack:
                    l[1].append(t.content)
            elif ty == "code_inline":
                f["code_inline"].append(t.content)
                textbuf.append(t.content)
                for s in open_spans:
                    s[1].append(t.content)
                for l in link_stack:
                    l[1].append(t.content)
            elif ty in ("strong_open", "em_open", "s_open"):
                open_spans.append((ty[:-5], []))
            elif ty in ("strong_close", "em_close", "s_close"):
                name = ty[:-6]
                for i in range(len(open_spans) - 1, -1, -1):
                    if open_spans[i][0] == name:
                        kind, parts = open_spans.pop(i)
                        f[{"strong": "strong", "em": "em", "s": "strike"}[kind]].append(norm("".join(parts)))
                        break
            elif ty == "link_open":
                link_stack.append((t.attrs.get("href", ""), []))
            elif ty == "link_close" and link_stack:
                href, parts = link_stack.pop()
                f["link"].append((norm("".join(parts)), href))
            elif ty == "image":
                f["image"].append((norm(t.content or "".join(c.content for c in t.children or [])), t.attrs.get("src", "")))
            elif ty in ("hardbreak", "softbreak"):
                if ty == "hardbreak":
                    f["break"].append(1)
                textbuf.append(" ")
                for s in open_spans:
                    s[1].append(" ")
                for l in link_stack:
                    l[1].append(" ")
        text = norm("".join(textbuf))
        f["text"].extend(text.split())
        return text

    list_depth = -1
    ordered = []
    quote = 0
    for t in toks:
        ty = t.type
        if ty in ("bullet_list_open", "ordered_list_open"):
            list_depth += 1
            ordered.append(ty == "ordered_list_open")
        elif ty in ("bullet_list_close", "ordered_list_close"):
            list_depth -= 1
            ordered.pop()
        elif ty == "list_item_open":
            stack.append(["item", None])
        elif ty == "list_item_close":
            item = stack.pop()
            f["list_item"].append((list_depth, ordered[-1], item[1] or ""))
        elif ty == "blockquote_open":
            quote += 1
        elif ty == "blockquote_close":
            quote -= 1
        elif ty == "heading_open":
            pending_heading = int(t.tag[1])
        elif ty == "table_open":
            table += 1
            row = -1
        elif ty == "tr_open":
            row += 1
            col = -1
        elif ty in ("th_open", "td_open"):
            col += 1
        elif ty in ("fence", "code_block"):
            lang = (t.info or "").strip().split(" ")[0] if ty == "fence" else ""
            f["code_block"].append((lang, t.content.rstrip("\n")))
        elif ty == "hr":
            f["hr"].append(1)
        elif ty == "inline":
            text = inline_feats(t.children or [])
            if pending_heading:
                f["heading"].append((pending_heading, text))
                pending_heading = None
            elif _in_cell[0]:
                f["table"].append((row, col, text))
            elif stack and stack[-1][0] == "item" and stack[-1][1] is None:
                stack[-1][1] = text
            elif quote and not stack:
                f["quote"].append((quote, text))
        if ty in ("th_open", "td_open"):
            _in_cell[0] = True
        elif ty in ("th_close", "td_close"):
            _in_cell[0] = False
        elif ty == "table_close":
            row = -1
    return {k: v for k, v in f.items()}


_in_cell = [False]


def prf(gold, got):
    g, o = collections.Counter(map(_key, gold)), collections.Counter(map(_key, got))
    tp = sum((g & o).values())
    p = tp / sum(o.values()) if o else (1.0 if not g else 0.0)
    r = tp / sum(g.values()) if g else 1.0
    f1 = 2 * p * r / (p + r) if p + r else 0.0
    return p, r, f1


def _href(u):
    """链接地址的比较形式：百分号解码，主机名按 IDNA 解码（解析器会把中文域名和路径编码）。"""
    from urllib.parse import unquote, urlsplit, urlunsplit
    try:
        sp = urlsplit(u)
        host = sp.netloc
        if "xn--" in host:
            host = host.encode().decode("idna")
        return unquote(urlunsplit((sp.scheme, host, sp.path, sp.query, sp.fragment)))
    except Exception:
        return u


def _key(x):
    if isinstance(x, tuple):
        return tuple(_href(v) if isinstance(v, str) and (v.startswith(("http", "/"))) else norm(v) if isinstance(v, str) else v for v in x)
    return norm(x) if isinstance(x, str) else x


CATS = ["heading", "text", "strong", "em", "strike", "code_inline", "link", "image", "break",
        "list_item", "table", "code_block", "quote", "hr", "math"]


def main():
    cmd = sys.argv[1]
    d = sys.argv[2]
    if cmd == "gen":
        n = int(sys.argv[3]) if len(sys.argv) > 3 else 200
        seed = int(sys.argv[4]) if len(sys.argv) > 4 else 1
        os.makedirs(d, exist_ok=True)
        gold = {}
        for i in range(n):
            s = seed * 100_003 + i
            blocks = Gen(s).doc()
            name = f"{i:03d}"
            with open(os.path.join(d, name + ".html"), "w", encoding="utf-8") as fh:
                fh.write(Render(s).page(blocks))
            gold[name] = gold_features(blocks)
        json.dump(gold, open(os.path.join(d, "gold.json"), "w"), ensure_ascii=False)
        ref = os.path.join(d, "ref")
        os.makedirs(ref, exist_ok=True)
        for i in range(n):
            blocks = Gen(seed * 100_003 + i).doc()
            with open(os.path.join(ref, f"{i:03d}.md"), "w", encoding="utf-8") as fh:
                fh.write("".join(md_block(b) for b in blocks))
        print(f"{n} documents in {d}")
        return
    gold = json.load(open(os.path.join(d, "gold.json")))
    gold = {k: {c: [tuple(x) if isinstance(x, list) else x for x in v] for c, v in g.items()} for k, g in gold.items()}
    print(f"{'tool':14} " + " ".join(f"{c[:9]:>9}" for c in CATS) + f" {'mean F1↑':>9}")
    for arg in sys.argv[3:]:
        tool, out = arg.split("=", 1)
        sums = collections.defaultdict(lambda: [0, 0])
        for name, g in gold.items():
            path = os.path.join(out, name + ".md")
            md = open(path, encoding="utf-8").read() if os.path.exists(path) else ""
            o = md_features(md)
            for c in CATS:
                if c in g or c in o:
                    gc, oc = collections.Counter(map(_key, g.get(c, []))), collections.Counter(map(_key, o.get(c, [])))
                    tp = sum((gc & oc).values())
                    sums[c][0] += tp
                    sums[c].append((sum(gc.values()), sum(oc.values())))
        row = []
        for c in CATS:
            v = sums[c]
            tp = v[0]
            G = sum(x[0] for x in v[2:])
            O = sum(x[1] for x in v[2:])
            p = tp / O if O else 0.0
            r = tp / G if G else 0.0
            row.append(2 * p * r / (p + r) if p + r else 0.0)
        print(f"{tool:14} " + " ".join(f"{x:9.3f}" for x in row) + f" {sum(row) / len(row):9.3f}")


if __name__ == "__main__":
    main()

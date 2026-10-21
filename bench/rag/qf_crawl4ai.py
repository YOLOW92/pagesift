"""Crawl4AI 的 BM25ContentFilter（默认参数：阈值 1.0、Snowball 词干化）在每个问题所属的文档上的输出，
映射回原文的字符区间。文档按空行分段转成 <p>（Crawl4AI 的过滤器吃 HTML）。
python -I qf_crawl4ai.py <语料目录> <问题.jsonl> <输出.json>   （在 Crawl4AI 的环境里跑）"""
import html, json, os, re, sys, time
from crawl4ai.content_filter_strategy import BM25ContentFilter

d, qpath, out = sys.argv[1:4]
texts = {f[:-3]: open(os.path.join(d, f), encoding="utf-8").read() for f in os.listdir(d)}
docs = {}
for name, t in texts.items():
    paras = [(m.start(), m.end()) for m in re.finditer(r"\S(?:.*?\S)?(?=\n\s*\n|\s*$)", t, re.S)]
    docs[name] = (paras, "<html><body>" + "".join(f"<p>{html.escape(t[a:b])}</p>" for a, b in paras) + "</body></html>")


def locate(t, paras, piece):
    plain = re.sub(r"<[^>]+>", " ", piece)
    plain = html.unescape(" ".join(plain.split()))
    for a, b in paras:
        if plain[:80] and plain[:80] in " ".join(t[a:b].split()):
            return [a, b]
    return None


res, ms = {}, 0.0
for line in open(qpath):
    q = json.loads(line)
    paras, page = docs[q["corpus"]]
    f = BM25ContentFilter(user_query=q["question"])
    t0 = time.perf_counter()
    kept = f.filter_content(page)
    ms += (time.perf_counter() - t0) * 1000
    spans = [s for s in (locate(texts[q["corpus"]], paras, k) for k in kept) if s]
    res[q["id"]] = spans
json.dump({"crawl4ai-default": res}, open(out, "w"))
print(f"crawl4ai: {ms / max(len(res), 1):.1f} ms/question", file=sys.stderr)

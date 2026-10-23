"""竞品分块器：把每个语料分块，输出每块在原文里的字符区间。

python -I chunkers.py <输出目录> <目标词数> <方法>... -- <语料.md>...
方法：langchain-recursive、langchain-markdown、chonkie-recursive、chonkie-markdown、chonkie-sentence、
chonkie-token、semchunk（在装了这些库的环境里），crawl4ai-fixed、crawl4ai-sliding、crawl4ai-regex
（在 Crawl4AI 的环境里）。块大小一律按空白分词的词数计（同一把尺子）。
输出 <输出目录>/<方法>.json：{"语料名": [[起, 止], ...]}。只给文本、不给位置的分块器，按顺序在原文里找回位置。
"""
import json, os, sys, time


def words(t):
    return len(t.split())


def locate(text, pieces):
    """按顺序在原文里找回每块的位置（允许重叠：下一块从上一块的起点之后找）。找不到时用开头 60 个字符找。"""
    spans, cursor = [], 0
    for p in pieces:
        p = p.strip()
        if not p:
            continue
        i = text.find(p, cursor)
        if i < 0:
            i = text.find(p[:60], cursor)
            if i < 0:
                i = text.find(p[:60])
            if i < 0:
                continue
            spans.append([i, min(len(text), i + len(p))])
        else:
            spans.append([i, i + len(p)])
        cursor = i + 1
    return spans


def make(method, size):
    if method == "langchain-recursive":
        from langchain_text_splitters import RecursiveCharacterTextSplitter
        s = RecursiveCharacterTextSplitter(chunk_size=size, chunk_overlap=0, length_function=words, add_start_index=True)
        return lambda t: [[d.metadata["start_index"], d.metadata["start_index"] + len(d.page_content)] for d in s.create_documents([t])]
    if method == "langchain-markdown":
        from langchain_text_splitters import Language, RecursiveCharacterTextSplitter
        s = RecursiveCharacterTextSplitter.from_language(Language.MARKDOWN, chunk_size=size, chunk_overlap=0, length_function=words, add_start_index=True)
        return lambda t: [[d.metadata["start_index"], d.metadata["start_index"] + len(d.page_content)] for d in s.create_documents([t])]
    if method.startswith("chonkie-"):
        import chonkie
        kind = method.split("-", 1)[1]
        if kind == "recursive":
            c = chonkie.RecursiveChunker(tokenizer="word", chunk_size=size)
        elif kind == "markdown":
            c = chonkie.RecursiveChunker.from_recipe("markdown", tokenizer="word", chunk_size=size)
        elif kind == "sentence":
            c = chonkie.SentenceChunker(tokenizer="word", chunk_size=size, chunk_overlap=0)
        else:
            c = chonkie.TokenChunker(tokenizer="word", chunk_size=size, chunk_overlap=0)
        return lambda t: [[ch.start_index, ch.end_index] for ch in c.chunk(t)]
    if method == "semchunk":
        import semchunk
        def f(t):
            _, offsets = semchunk.chunk(t, chunk_size=size, token_counter=words, offsets=True)
            return [list(o) for o in offsets]
        return f
    if method == "llamaindex-sentence":
        from llama_index.core.node_parser import SentenceSplitter
        s = SentenceSplitter(chunk_size=size, chunk_overlap=0)
        return lambda t: locate(t, s.split_text(t))
    if method.startswith("crawl4ai-"):
        from crawl4ai import chunking_strategy as cs
        kind = method.split("-", 1)[1]
        if kind == "fixed":
            c = cs.FixedLengthWordChunking(chunk_size=size)
        elif kind == "sliding":
            c = cs.SlidingWindowChunking(window_size=size, step=size // 2)
        else:
            c = cs.RegexChunking()
        return lambda t: locate(t, c.chunk(t))
    raise SystemExit(f"unknown method {method}")


def main():
    out, size = sys.argv[1], int(sys.argv[2])
    sep = sys.argv.index("--")
    methods, corpora = sys.argv[3:sep], sys.argv[sep + 1:]
    os.makedirs(out, exist_ok=True)
    texts = {os.path.splitext(os.path.basename(p))[0]: open(p, encoding="utf-8").read() for p in corpora}
    for m in methods:
        try:
            f = make(m, size)
        except Exception as e:
            print(f"{m}: unavailable ({e})", file=sys.stderr)
            continue
        t0 = time.perf_counter()
        res = {name: f(text) for name, text in texts.items()}
        ms = (time.perf_counter() - t0) * 1000
        json.dump(res, open(os.path.join(out, m + ".json"), "w"))
        print(f"{m}: {sum(len(v) for v in res.values())} chunks, {ms:.1f} ms", file=sys.stderr)


main()

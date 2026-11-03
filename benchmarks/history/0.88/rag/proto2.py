"""原型 2：与 Rust 同样的 token 计数（拉丁文按空白、中日韩每字一个），合并策略可换。

python -I proto2.py <策略> <size> <输出.json> <语料...>
"""
import bisect, json, os, re, sys

PUNCT = [".", "。", "?", "？", "!", "！", ";", "；", ",", "，", "、", "(", ")", "[", "]", "“", "”", "‘",
         "’", "'", "\"", "`", "*", ":", "：", "—", "…", "/", "\\", "–", "&", "-"]
CJK = ("ᄀ-ᇿ⺀-⿟　-〿぀-ヿ㄰-㆏ㇰ-ㇿ㐀-䶿"
       "一-鿿가-힯豈-﫿＀-￯\U00020000-\U0002FA1F")
TOK = re.compile(f"[{CJK}]|[^\\s{CJK}]+")


class Counter:
    def __init__(self, text):
        self.starts = [m.start() for m in TOK.finditer(text)]

    def __call__(self, a, b):
        return bisect.bisect_left(self.starts, b) - bisect.bisect_left(self.starts, a)


def longest_run(s, chars):
    best = None
    for m in re.finditer("[" + re.escape("".join(chars)) + "]+", s):
        if best is None or len(m.group()) > len(best):
            best = m.group()
    return best


def split(text, a, b):
    s = text[a:b]

    def by(sep):
        out, start = [], 0
        i = s.find(sep)
        while i >= 0:
            out.append((a + start, a + i))
            start = i + len(sep)
            i = s.find(sep, start)
        out.append((a + start, a + len(s)))
        return out

    if "\n" in s or "\r" in s:
        sep = longest_run(s, ["\n", "\r"])
        return by(sep), "nl"
    if "\t" in s:
        return by(longest_run(s, ["\t"])), "tab"
    m = re.search(r"\s", s)
    if m:
        run = longest_run(s, [" ", " ", "\x0b", "\x0c"])
        if run and len(run) >= 2:
            return by(run), "ws"
        for p in PUNCT:
            cuts, frm = [], 0
            while True:
                i = s.find(p, frm)
                if i < 0:
                    break
                at = i + len(p)
                if at < len(s) and s[at].isspace():
                    cuts.append(at)
                frm = i + len(p)
            if cuts:
                out, start = [], 0
                for at in cuts:
                    out.append((a + start, a + at))
                    start = at + 1
                out.append((a + start, a + len(s)))
                return out, "sent"
        return by(s[m.start()]), "ws"
    raise RuntimeError("no-ws pieces not expected in this corpus")


def fill(t, cap):
    out, i = [], 0
    while i < len(t):
        acc, j = t[i], i + 1
        while j < len(t) and acc + t[j] <= cap:
            acc += t[j]
            j += 1
        out.append((i, j))
        i = j
    return out


def minmax(t, cap):
    k = len(fill(t, cap))
    lo, hi = min(max(t), cap), cap
    while lo < hi:
        mid = (lo + hi) // 2
        if len(fill(t, mid)) <= k:
            hi = mid
        else:
            lo = mid + 1
    return fill(t, lo)


POLICIES = {}


def policy(f):
    POLICIES[f.__name__] = f
    return f


@policy
def greedy(t, ctx):
    return fill(t, ctx["size"])


@policy
def bal(t, ctx):
    return minmax(t, ctx["size"])


@policy
def cur(t, ctx):  # 0.88 第一版
    return minmax(t, min(ctx["size"], max(ctx["size"] // 2, max(t))))


def run_chunks(text, cnt, a, b, size, pol, ctx, out, depth=0):
    if cnt(a, b) <= size:
        out.append((a, b))
        return
    pieces, lvl = split(text, a, b)
    if len(pieces) <= 1:
        out.append((a, b))
        return
    t = [cnt(x, y) for x, y in pieces]
    c = dict(ctx, size=size, depth=depth, lvl=lvl, level_tokens=t)
    i = 0
    while i < len(pieces):
        if t[i] > size:
            run_chunks(text, cnt, pieces[i][0], pieces[i][1], size, pol, ctx, out, depth + 1)
            i += 1
            continue
        j = i
        while j < len(pieces) and t[j] <= size:
            j += 1
        for gi, gj in pol(t[i:j], c):
            out.append((pieces[i + gi][0], pieces[i + gj - 1][1]))
        i = j


def doc_stats(text, cnt, size):
    """顶层块（第一层切分的片段）的长度。"""
    pieces, _ = split(text, 0, len(text))
    return [cnt(x, y) for x, y in pieces if cnt(x, y) > 0]


def chunk_doc(text, size, pol):
    cnt = Counter(text)
    ctx = {"top": doc_stats(text, cnt, size)}
    out = []
    run_chunks(text, cnt, 0, len(text), size, pol, ctx, out)
    spans = []
    for a, b in out:
        s = text[a:b]
        if not s.strip():
            continue
        lead = len(s) - len(s.lstrip())
        spans.append([a + lead, a + lead + len(s.strip())])
    return spans


def main():
    pol, size, outp = POLICIES[sys.argv[1]], int(sys.argv[2]), sys.argv[3]
    res = {}
    for path in sys.argv[4:]:
        res[os.path.splitext(os.path.basename(path))[0]] = chunk_doc(open(path, encoding="utf-8").read(), size, pol)
    json.dump(res, open(outp, "w"), sort_keys=True)


def _half(t, ctx):
    return min(ctx["size"], max(ctx["size"] // 2, max(t)))


@policy
def curg(t, ctx):  # 半预算上限，贪心
    return fill(t, _half(t, ctx))


@policy
def nl_cur_in_bal(t, ctx):  # 块之间半预算均衡，块内部按预算均衡
    return minmax(t, _half(t, ctx)) if ctx["lvl"] == "nl" else minmax(t, ctx["size"])


@policy
def nl_cur_in_greedy(t, ctx):  # 块之间半预算均衡，块内部 0.81 贪心
    return minmax(t, _half(t, ctx)) if ctx["lvl"] == "nl" else fill(t, ctx["size"])


@policy
def nl_greedy_in_cur(t, ctx):  # 块之间 0.81 贪心，块内部半预算均衡
    return fill(t, ctx["size"]) if ctx["lvl"] == "nl" else minmax(t, _half(t, ctx))


@policy
def nl_max_in_bal(t, ctx):  # 块之间只合并到这一串最长块那么长，块内部按预算均衡（切得最少）
    return minmax(t, max(t)) if ctx["lvl"] == "nl" else minmax(t, ctx["size"])


@policy
def nl_max_in_greedy(t, ctx):
    return minmax(t, max(t)) if ctx["lvl"] == "nl" else fill(t, ctx["size"])


def _whole_scale(ctx):
    w = [x for x in ctx["top"] if x <= ctx["size"]]
    if not w:
        return ctx["size"]
    return sum(x * x for x in w) / sum(w)


@policy
def nl_cur_in_scale(t, ctx):  # 块之间半预算均衡；块内部切成像文档里整块那么长的部分（至少半预算）
    if ctx["lvl"] == "nl":
        return minmax(t, _half(t, ctx))
    cap = min(ctx["size"], max(ctx["size"] // 2, max(t), round(_whole_scale(ctx))))
    return minmax(t, cap)


if __name__ == "__main__":
    main()

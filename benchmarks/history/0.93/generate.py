"""Build relocation cases for 0.93 from the corpus collected by fetch.py.

Each case: an original page, a selector a scraper author would write for one element, a changed page,
and the expected text of the same element in the changed page.

- synthetic: live pages with one redesign applied (hashed class names, an extra wrapper, a changed
  tag, removed attributes, inserted look-alike siblings, and all three structural changes combined);
- real: the same URL in two Common Crawl crawls years apart; the target is an element whose text
  occurs as the whole text of exactly one element in the newer page (that element is the answer).

Pages are split into a tuning half and a holdout half by a seeded hash of the URL; the method has no
tunable thresholds (δ = 0.05 is fixed), the tuning half was only looked at while developing.

    python -I generate.py   # writes benchmarks/data/0.93/cases.jsonl
"""

import hashlib
import json
import pathlib
import random
import re
import urllib.parse

import lxml.html
from lxml.cssselect import CSSSelector

DATA = pathlib.Path(__file__).resolve().parents[2] / "data" / "0.93"
SEED = 930
PER_PAGE = 8
SKIP = {"script", "style", "title", "head", "noscript", "template", "option", "html", "body", "meta", "link", "br", "svg", "path"}
MUTATIONS = ["hash_classes", "wrap", "retag", "drop_attrs", "insert_siblings", "combo"]
RETAG = {"span": "div", "div": "section", "h1": "h2", "h2": "h3", "h3": "h4", "h4": "h5", "p": "div", "li": "div",
         "a": "span", "b": "strong", "strong": "b", "em": "i", "td": "div", "dd": "div", "dt": "div", "small": "span", "label": "span", "button": "a"}


def norm(t):
    return re.sub(r"\s+", " ", t or "").strip()


def key(t):
    return re.sub(r"\s+", "", t or "")


def split_of(url):
    h = hashlib.sha256(f"{SEED}:{url}".encode()).digest()[0]
    return "tune" if h % 2 == 0 else "holdout"


def minimal_text_elements(doc):
    out = []
    for el in doc.iter():
        if not isinstance(el.tag, str) or el.tag in SKIP:
            continue
        t = norm(el.text_content())
        if not (3 <= len(t) <= 120):
            continue
        if any(isinstance(c.tag, str) and norm(c.text_content()) == t for c in el):
            continue
        out.append((el, t))
    return out


def hashy(v):
    return bool(re.search(r"\d[a-z]\d|[a-z]\d[a-z]", v))


def step(el):
    s = el.tag
    cls = [c for c in (el.get("class") or "").split() if re.fullmatch(r"[A-Za-z_][\w-]*", c)]
    if cls:
        s += "." + cls[0]
    parent = el.getparent()
    if parent is not None:
        same = [c for c in parent if c.tag == el.tag]
        if len(same) > 1:
            s += f":nth-of-type({same.index(el) + 1})"
    return s


def user_selector(el, doc):
    """A selector like a scraper author writes: from the nearest id, tag.class steps, nth-of-type if needed."""
    parts = []
    cur = el
    while cur is not None and isinstance(cur.tag, str) and cur.tag not in ("html", "body"):
        i = cur.get("id")
        if i and re.fullmatch(r"[A-Za-z][\w-]*", i) and cur is not el:
            parts.append("#" + i)
            break
        parts.append(step(cur))
        cur = cur.getparent()
        if len(parts) >= 5:
            break
    sel = " > ".join(reversed(parts))
    # lxml 不按 HTML5 补 <tbody>：`table > tr` 在浏览器和 HTML5 解析器里本来就不命中，这种选择器
    # 作者在开发者工具里写不出来，剔除（对两边一样）
    if re.search(r"\btable(\.[\w-]+)?(:nth-of-type\(\d+\))? > tr\b", sel):
        return None
    try:
        hits = CSSSelector(sel)(doc)
    except Exception:
        return None
    return sel if len(hits) == 1 and hits[0] is el else None


def serialize(doc):
    return lxml.html.tostring(doc, encoding="unicode")


def mutate(html, target_marker, kind, rng):
    doc = lxml.html.fromstring(html)
    target = doc.xpath(f"//*[@data-bench-target='{target_marker}']")[0]
    kinds = ["hash_classes", "wrap", "insert_siblings"] if kind == "combo" else [kind]
    for k in kinds:
        if k == "hash_classes":
            for el in doc.iter():
                if isinstance(el.tag, str) and el.get("class"):
                    el.set("class", " ".join("css-" + hashlib.sha1(f"{SEED}{c}".encode()).hexdigest()[:2] + "x" + hashlib.sha1(c.encode()).hexdigest()[2:6] for c in el.get("class").split()))
        elif k == "wrap":
            parent = target.getparent()
            gp = parent.getparent() if parent is not None else None
            if gp is not None:
                w = lxml.html.Element("div")
                w.set("class", "layout")
                gp.replace(parent, w)
                w.append(parent)
        elif k == "retag":
            target.tag = RETAG.get(target.tag, "div")
        elif k == "drop_attrs":
            cur = target
            for _ in range(4):
                if cur is None or not isinstance(cur.tag, str):
                    break
                for a in ("id", "class"):
                    if a in cur.attrib:
                        del cur.attrib[a]
                cur = cur.getparent()
        elif k == "insert_siblings":
            for i in range(2):
                sib = lxml.html.Element(target.tag)
                for a, v in target.attrib.items():
                    if a != "data-bench-target":
                        sib.set(a, v)
                sib.text = f"Sponsored {rng.randint(100, 999)} {target.tag}"
                target.addprevious(sib)
    del target.attrib["data-bench-target"]
    return serialize(doc)


def synthetic(rng):
    cases = []
    for path in sorted((DATA / "live").glob("*.html")):
        url = urllib.parse.unquote(path.stem)
        html = path.read_bytes().decode("utf-8", "replace")
        try:
            doc = lxml.html.fromstring(html)
        except Exception:
            continue
        cands = minimal_text_elements(doc)
        counts = {}
        for _, t in cands:
            counts[key(t)] = counts.get(key(t), 0) + 1
        cands = [(e, t) for e, t in cands if counts[key(t)] == 1]
        rng.shuffle(cands)
        chosen = []
        for el, t in cands:
            sel = user_selector(el, doc)
            if sel:
                chosen.append((el, t, sel))
            if len(chosen) >= PER_PAGE:
                break
        for n, (el, t, sel) in enumerate(chosen):
            el.set("data-bench-target", str(n))
        marked = serialize(doc)
        for el, _, _ in chosen:
            del el.attrib["data-bench-target"]
        original = serialize(doc)
        for n, (el, t, sel) in enumerate(chosen):
            # 只标记当前目标
            single = re.sub(r' data-bench-target="(?!%d")\d+"' % n, "", marked)
            for kind in MUTATIONS:
                cases.append({
                    "id": f"syn:{path.stem[:40]}:{n}:{kind}",
                    "split": split_of(url), "kind": kind, "source": "synthetic", "url": url,
                    "original_html": original, "selector": sel,
                    "mutated_html": mutate(single, n, kind, rng), "expected": t,
                })
    return cases


def real(rng):
    cases = []
    for path in sorted((DATA / "cc").glob("*.json")):
        pair = json.loads(path.read_text())
        try:
            old = lxml.html.fromstring(pair["old"])
            new = lxml.html.fromstring(pair["new"])
        except Exception:
            continue
        new_counts = {}
        for _, t in minimal_text_elements(new):
            new_counts[key(t)] = new_counts.get(key(t), 0) + 1
        cands = minimal_text_elements(old)
        old_counts = {}
        for _, t in cands:
            old_counts[key(t)] = old_counts.get(key(t), 0) + 1
        cands = [(e, t) for e, t in cands if old_counts[key(t)] == 1 and new_counts.get(key(t)) == 1]
        rng.shuffle(cands)
        n = 0
        for el, t in cands:
            sel = user_selector(el, old)
            if not sel:
                continue
            cases.append({
                "id": f"real:{path.stem[:40]}:{n}", "split": split_of(pair["url"]), "kind": "real",
                "source": "common_crawl", "url": pair["url"],
                "original_html": pair["old"], "selector": sel, "mutated_html": pair["new"], "expected": t,
            })
            n += 1
            if n >= PER_PAGE:
                break
    return cases


if __name__ == "__main__":
    rng = random.Random(SEED)
    cases = synthetic(rng) + real(rng)
    with open(DATA / "cases.jsonl", "w") as f:
        for c in cases:
            f.write(json.dumps(c) + "\n")
    by = {}
    for c in cases:
        by[(c["split"], c["kind"])] = by.get((c["split"], c["kind"]), 0) + 1
    print(len(cases), "cases", dict(sorted(by.items())))

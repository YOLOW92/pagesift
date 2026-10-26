"""混合增强基准的抽样（RULES.md 第二、三节）。只用标准库：python3 -I bench/mix/sample.py <子类|all> [--out DIR]

规则冻结前不许执行：seeds.toml 里 frozen = false 时直接退出。抽样只按公开列表和固定种子，抽到什么用什么。
输出 <out>/<子类>.tsv：id、来源、位置（本地相对路径或 URL）、备注。网页快照（WARC）和 B2 的文件下载是后面的
snapshot 步骤，不在这里；B2 这里只调各来源的公开列表接口拿候选，再按种子抽。

数据位置用环境变量指定（默认是本机 pagesift 主检出下的 bench/data）：
  MIX_DATA        bench/data
  MIX_OFFICE      修复 3 的 Office 语料根目录（下有 dev/ holdout/）
"""
import argparse
import hashlib
import json
import os
import random
import sys
import tomllib
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONF = tomllib.loads((HERE / "seeds.toml").read_text())
SEEDS, QUOTA = CONF["seeds"], CONF["quota"]
DATA = Path(os.environ.get("MIX_DATA", Path.home() / "pagesift/bench/data"))
OFFICE = Path(os.environ.get("MIX_OFFICE", Path.home() / "pagesift/tasks/0.80/fix3/office"))
UA = "pagesift-mix-bench/0.80 (+https://github.com/YOLOW92/pagesift)"


def pick(seed, items, k):
    """固定种子从排好序的候选里不放回抽 k 个；候选不够 k 个就全要。结果按候选原顺序排。"""
    items = sorted(items)
    if len(items) <= k:
        return items
    idx = sorted(random.Random(seed).sample(range(len(items)), k))
    return [items[i] for i in idx]


def in_subset(seed, key, fraction):
    """与顺序无关的稳定子集：sha256(种子:id) 落在前 fraction 内。用于“用模型的工具只跑 10%”。"""
    h = int.from_bytes(hashlib.sha256(f"{seed}:{key}".encode()).digest()[:8], "big")
    return h < fraction * 2**64


def get_json(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read())


def get_text(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("utf-8", "replace")


# ---- A1 已标注正文：四套公开标注集各抽 150 ----

def a1():
    n = QUOTA["a1_per_set"]
    rows = []
    aeb = [p.name for p in (DATA / "fresh/data/aeb/html").glob("*.html")]
    rows += [(f"aeb-{x}", "AEB", f"fresh/data/aeb/html/{x}", "") for x in pick(SEEDS["a1_aeb"], aeb, n)]
    # WCXB 只用 test 划分：dev 划分在开发时看过
    wcxb = [p.name for p in (DATA / "fresh/data/wcxb/test/html").glob("*")]
    rows += [(f"wcxb-{x}", "WCXB test", f"fresh/data/wcxb/test/html/{x}", "") for x in pick(SEEDS["a1_wcxb"], wcxb, n)]
    wmb = []
    with open(DATA / "p4_2_webmainbench/webmainbench.jsonl", encoding="utf-8") as f:
        for i, line in enumerate(f):
            r = json.loads(line)
            wmb.append(str(r.get("track_id") or r.get("id") or i))
    rows += [(f"wmb-{x}", "WebMainBench", f"p4_2_webmainbench/webmainbench.jsonl#{x}", "") for x in pick(SEEDS["a1_webmainbench"], wmb, n)]
    fc = []
    with open(DATA / "inputs/manifest.jsonl", encoding="utf-8") as f:
        for line in f:
            fc.append(str(json.loads(line)["id"]))
    rows += [(f"fc-{x}", "Firecrawl scrape-content-dataset-v1", f"inputs/manifest.jsonl#{x}", "") for x in pick(SEEDS["a1_firecrawl"], fc, n)]
    return rows


# ---- A2 随机真实网页：Tranco 五层各 80 站 + 五种语言各 20 站 ----

def tranco():
    out = []
    with open(DATA / "top-1m.csv", encoding="utf-8") as f:
        for line in f:
            rank, dom = line.strip().split(",", 1)
            out.append((int(rank), dom))
    return out


def a2():
    ranks = tranco()
    a2c = CONF["a2"]
    rng_kind = random.Random(SEEDS["a2_tranco"] + 1)
    rows, taken = [], set()
    for lo, hi in a2c["tiers"]:
        cands = [f"{r:07d}\t{d}" for r, d in ranks if lo <= r < hi]
        for c in pick(SEEDS["a2_tranco"], cands, QUOTA["a2_per_tier"]):
            r, d = c.split("\t")
            taken.add(d)
            kind = "home" if rng_kind.random() < 0.5 else "inner"  # 首页或一个随机内链（内链在抓首页后按 a2_inner_link 种子挑）
            rows.append((f"a2-{int(r)}-{d}", f"Tranco {lo}-{hi - 1}", f"https://{d}/", kind))
    for lang, tlds in a2c["lang_tlds"].items():
        cands = [f"{r:07d}\t{d}" for r, d in ranks if d.rsplit(".", 1)[-1] in tlds and d not in taken]
        for c in pick(SEEDS["a2_lang"] + sum(map(ord, lang)), cands, QUOTA["a2_per_lang"]):
            r, d = c.split("\t")
            rows.append((f"a2{lang}-{int(r)}-{d}", f"Tranco ccTLD {lang}", f"https://{d}/", "home"))
    return rows


# ---- A4 难题：五个来源各 40 ----

def a4():
    n = QUOTA["a4_per_source"]
    s = SEEDS["a4_hard"]
    rows = []
    enc = [p.relative_to(DATA).as_posix() for p in (DATA / "fresh/data/chardet-test-data").rglob("*") if p.is_file() and p.suffix in (".html", ".htm", ".txt")]
    rows += [(f"a4enc-{i}", "chardet-test-data（编码陷阱）", x, "") for i, x in enumerate(pick(s, enc, n))]
    wpt = [p.relative_to(DATA).as_posix() for p in (DATA / "wpt-parsing").rglob("*.dat")]
    rows += [(f"a4wpt-{i}", "WPT html5lib（残缺 HTML，整页化在 snapshot 步骤）", x, "") for i, x in enumerate(pick(s + 1, wpt, n))]
    snaps = [p for d in ("snapshots", "snapshots-b") for p in (DATA / d).glob("*.html")]
    big = [p.relative_to(DATA).as_posix() for p in snaps if p.stat().st_size > 5 * 2**20]
    rows += [(f"a4big-{i}", "已抓快照中 > 5 MB 的页面", x, "") for i, x in enumerate(pick(s + 2, big, n))]
    blocked = [p.relative_to(DATA).as_posix() for p in (DATA / "showdown").rglob("*.html")] if (DATA / "showdown").exists() else []
    rows += [(f"a4blk-{i}", "已抓到的拦截页、挑战页", x, "label=blocked") for i, x in enumerate(pick(s + 3, blocked, n))]
    # 深层嵌套：确定性生成（深度按种子取 1k–100k），snapshot 步骤写出文件
    rng = random.Random(s + 4)
    rows += [(f"a4deep-{i}", "生成的深层嵌套", f"generated:nest:{rng.randint(1000, 100000)}", "") for i in range(n)]
    return rows


# ---- B1 olmOCR 原生数字 PDF：全部，不抽 ----

def b1():
    root = DATA / "olmocr_1403/bench_data/pdfs"
    rows = []
    for cat in sorted(p.name for p in root.iterdir() if p.is_dir()):
        if cat.startswith("old_scans"):  # 扫描件不属于原生数字子集
            continue
        rows += [(f"b1-{cat}-{p.stem}", f"olmOCR-bench {cat}", p.relative_to(DATA).as_posix(), "") for p in sorted((root / cat).glob("*.pdf"))]
    return rows


# ---- B2 PDF 随机真实文件：四个来源各 25，候选来自各来源的公开列表接口 ----

def b2_arxiv(n, seed):
    # arXiv 公开 API：在 2010-01 到 2026-09 之间按种子抽月份，每月取该月按提交时间排序的前 50 篇作候选
    rng = random.Random(seed)
    cands = []
    for _ in range(n * 2):
        y, m = rng.randint(2010, 2026), rng.randint(1, 12)
        if (y, m) > (2026, 9):
            continue
        q = urllib.parse.urlencode({"search_query": f"submittedDate:[{y}{m:02d}010000 TO {y}{m:02d}282359]", "start": 0, "max_results": 50, "sortBy": "submittedDate"})
        xml = get_text(f"https://export.arxiv.org/api/query?{q}")
        cands += [seg.split("</id>")[0].rsplit("/abs/", 1)[-1] for seg in xml.split("<id>")[2:]]
    return [(f"b2arxiv-{x.replace('/', '_')}", "arXiv", f"https://arxiv.org/pdf/{x}", "") for x in pick(seed, set(cands), n)]


def b2_edgar(n, seed):
    # SEC EDGAR 季度索引：按种子抽 2005–2026 的季度，取其中 10-K 的主文件 PDF 附件作候选（多数年报是 HTML，只收 PDF）
    rng = random.Random(seed)
    cands = []
    for _ in range(n):
        y, q = rng.randint(2005, 2026), rng.randint(1, 4)
        idx = get_text(f"https://www.sec.gov/Archives/edgar/full-index/{y}/QTR{q}/form.idx")
        cands += [ln.split()[-1] for ln in idx.splitlines() if ln.startswith("10-K ")][:200]
    return [(f"b2edgar-{hashlib.sha1(x.encode()).hexdigest()[:12]}", "SEC EDGAR 10-K", f"https://www.sec.gov/Archives/{x}", "resolve-pdf-attachment") for x in pick(seed, set(cands), n)]


def b2_commons(n, seed):
    # 维基共享资源：allimages 按 MIME 过滤 PDF，从种子决定的随机起点往后取
    rng = random.Random(seed)
    cands = []
    for _ in range(n):
        start = "".join(rng.choice("ABCDEFGHIJKLMNOPQRSTUVWXYZ") for _ in range(3))
        q = urllib.parse.urlencode({"action": "query", "list": "allimages", "aimime": "application/pdf", "aifrom": start, "ailimit": 20, "aiprop": "url|size", "format": "json"})
        cands += [i["url"] for i in get_json(f"https://commons.wikimedia.org/w/api.php?{q}")["query"]["allimages"]]
    return [(f"b2commons-{hashlib.sha1(x.encode()).hexdigest()[:12]}", "Wikimedia Commons", x, "") for x in pick(seed, set(cands), n)]


def b2_gov(n, seed):
    # 政府公开报告：GOV.UK 搜索 API 里带 PDF 附件的 official_documents / research_and_statistics（还没定，见 RULES 第七节）
    rng = random.Random(seed)
    cands = []
    for _ in range(n):
        q = urllib.parse.urlencode({"filter_content_purpose_supergroup": "research_and_statistics", "count": 20, "start": rng.randint(0, 50000), "fields": "link"})
        cands += [r["link"] for r in get_json(f"https://www.gov.uk/api/search.json?{q}")["results"]]
    return [(f"b2gov-{hashlib.sha1(x.encode()).hexdigest()[:12]}", "GOV.UK", f"https://www.gov.uk{x}", "resolve-pdf-attachment") for x in pick(seed, set(cands), n)]


def b2():
    n = QUOTA["b2_per_source"]
    return b2_arxiv(n, SEEDS["b2_arxiv"]) + b2_gov(n, SEEDS["b2_gov"]) + b2_edgar(n, SEEDS["b2_edgar"]) + b2_commons(n, SEEDS["b2_commons"])


# ---- B3 PDF 难题：pdf.js 测试集（本地文件，不含 .link 外链）按种子抽 100 ----

def b3():
    root = DATA / "pdfjs/test/pdfs"
    cands = [p.relative_to(DATA).as_posix() for p in root.glob("*.pdf")]
    return [(f"b3-{Path(x).stem}", "pdf.js test/pdfs", x, "") for x in pick(SEEDS["b3_hard"], cands, QUOTA["b3_total"])]


# ---- B4 Office 与电子书：修复 3 的开发集 + 留出集，全部 ----

def b4():
    rows = []
    for split in ("dev", "holdout"):
        for p in sorted((OFFICE / split).rglob("*")):
            if p.is_file() and p.suffix.lower() in (".docx", ".xlsx", ".pptx", ".epub", ".csv"):
                rows.append((f"b4-{split}-{p.stem}", f"修复 3 {split}", f"{split}/{p.relative_to(OFFICE / split).as_posix()}", ""))
    return rows


def c1():
    sys.exit("C1：pagesift-fuzz 目前只记卡死/爆内存的次数，不保存样本，仓库里没有可用的样本集（见 RULES 第七节）")


SUBCLASSES = {"a1": a1, "a2": a2, "a4": a4, "b1": b1, "b2": b2, "b3": b3, "b4": b4, "c1": c1}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("which", choices=[*SUBCLASSES, "all"])
    ap.add_argument("--out", default=str(HERE / "samples"))
    a = ap.parse_args()
    if not CONF.get("frozen"):
        sys.exit("规则还没冻结（seeds.toml: frozen = false），不抽样。")
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    for name in (SUBCLASSES if a.which == "all" else [a.which]):
        rows = SUBCLASSES[name]()
        with open(out / f"{name}.tsv", "w", encoding="utf-8") as f:
            f.write("id\tsource\tlocation\tnote\tmodel_subset\n")
            for id_, src, loc, note in rows:
                sub = int(in_subset(SEEDS["model_subset"], id_, QUOTA["model_fraction"]))
                f.write(f"{id_}\t{src}\t{loc}\t{note}\t{sub}\n")
        print(f"{name}: {len(rows)} 份 → {out / f'{name}.tsv'}")


if __name__ == "__main__":
    main()

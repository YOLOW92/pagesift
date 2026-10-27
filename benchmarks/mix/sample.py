"""混合增强基准的抽样（RULES.md 第二、三节）。只用标准库：python3 -I benchmarks/mix/sample.py <子类|all> [--out DIR]

规则冻结前不许执行：seeds.toml 里 frozen = false 时直接退出。抽样只按公开列表和固定种子，抽到什么用什么。
输出 <out>/<子类>.tsv，列：id、来源、位置、原始 URL、备注、model_subset（1 = 用模型的工具也跑这一份）。
“位置”是本地数据的相对路径（相对 MIX_DATA，`tasks:` 开头的相对 pagesift 主检出），或要抓的 URL。
本地文件变成工具输入的那一步在 materialize.py，网络抓取（A2 快照、B2 下载）在 fetch.py。

数据位置（默认是本机 pagesift 主检出）：MIX_DATA = bench/data，MIX_OFFICE = 修复 3 的 Office 语料根目录，
MIX_TASKS = tasks/0.80。
"""
import argparse
import hashlib
import json
import os
import random
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONF = tomllib.loads((HERE / "seeds.toml").read_text())
SEEDS, QUOTA = CONF["seeds"], CONF["quota"]
CHECKOUT = Path(os.environ.get("PAGESIFT_CHECKOUT", Path.home() / "pagesift"))
DATA = Path(os.environ.get("MIX_DATA", CHECKOUT / "bench/data"))
OFFICE = Path(os.environ.get("MIX_OFFICE", CHECKOUT / "tasks/0.80/fix3/office"))
TASKS = Path(os.environ.get("MIX_TASKS", CHECKOUT / "tasks/0.80"))
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


_last = {}


def get(url, as_json=False, min_gap=1.0):
    """公开接口：同一主机两次请求至少隔 min_gap 秒（arXiv 要求 3 秒）。"""
    host = urllib.parse.urlsplit(url).hostname
    wait = _last.get(host, 0) + min_gap - time.monotonic()
    if wait > 0:
        time.sleep(wait)
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    for attempt in range(6):
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                body = r.read()
            break
        except urllib.error.HTTPError as e:
            # 429/503：按 Retry-After（没有就 15、30、60… 秒）退避后重试，最多 5 次
            if e.code not in (429, 503) or attempt == 5:
                raise
            ra = e.headers.get("Retry-After", "")
            time.sleep(int(ra) if ra.isdigit() else 15 * 2**attempt)
        finally:
            _last[host] = time.monotonic()
    return json.loads(body) if as_json else body.decode("utf-8", "replace")


def sha_id(prefix, s):
    return f"{prefix}-{hashlib.sha1(s.encode()).hexdigest()[:12]}"


# ---- A1 已标注正文：四套公开标注集各抽 150 ----

def a1():
    n = QUOTA["a1_per_set"]
    rows = []
    aeb = [p.name.split(".")[0] for p in (DATA / "fresh/data/aeb/html").glob("*.html*")]  # 文件是 <sha256>.html.gz
    rows += [(f"aeb-{x}", "AEB", f"fresh/data/aeb/html/{x}.html.gz", "", "") for x in pick(SEEDS["a1_aeb"], aeb, n)]
    # WCXB 只用 test 划分（dev 划分开发时看过）；原始 URL 在 ground-truth/<id>.json
    wcxb = [p.name.split(".")[0] for p in (DATA / "fresh/data/wcxb/test/html").glob("*.html*")]
    for x in pick(SEEDS["a1_wcxb"], wcxb, n):
        gt = json.loads((DATA / f"fresh/data/wcxb/test/ground-truth/{x}.json").read_text())
        html = next((DATA / "fresh/data/wcxb/test/html").glob(f"{x}.html*"))
        rows.append((f"wcxb-{x}", "WCXB test", html.relative_to(DATA).as_posix(), gt.get("url", ""), ""))
    wmb = {}
    with open(DATA / "p4_2_webmainbench/webmainbench.jsonl", encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            wmb[r["track_id"]] = r.get("url", "")
    rows += [(f"wmb-{x}", "WebMainBench", f"p4_2_webmainbench/webmainbench.jsonl#{x}", wmb[x], "") for x in pick(SEEDS["a1_webmainbench"], wmb, n)]
    fc = {}
    with open(DATA / "inputs/manifest.jsonl", encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            # path 形如 bench/data/inputs/1.html（相对 pagesift 主检出）；薄页用的是浏览器渲染快照，照清单用
            fc[str(r["id"])] = (r["path"].removeprefix("bench/data/"), r.get("url") or r.get("base_url") or "")
    rows += [(f"fc-{x}", "Firecrawl scrape-content-dataset-v1", fc[x][0], fc[x][1], "") for x in pick(SEEDS["a1_firecrawl"], fc, n)]
    return rows


# ---- A2 随机真实网页：Tranco 五层各 80 站 + 五种语言各 20 站 ----

def a2():
    ranks = []
    with open(DATA / "top-1m.csv", encoding="utf-8") as f:
        for line in f:
            r, d = line.strip().split(",", 1)
            ranks.append((int(r), d))
    a2c = CONF["a2"]
    rng_kind = random.Random(SEEDS["a2_tranco"] + 1)
    rows, taken = [], set()
    for lo, hi in a2c["tiers"]:
        cands = [f"{r:07d}\t{d}" for r, d in ranks if lo <= r < hi]
        for c in pick(SEEDS["a2_tranco"], cands, QUOTA["a2_per_tier"]):
            r, d = c.split("\t")
            taken.add(d)
            kind = "home" if rng_kind.random() < 0.5 else "inner"  # inner：抓首页后按 a2_inner_link 种子挑一条同站内链
            rows.append((f"a2-{int(r)}-{d}", f"Tranco {lo}-{hi - 1}", f"https://{d}/", "", kind))
    for lang, tlds in a2c["lang_tlds"].items():
        cands = [f"{r:07d}\t{d}" for r, d in ranks if d.rsplit(".", 1)[-1] in tlds and d not in taken]
        for c in pick(SEEDS["a2_lang"] + sum(map(ord, lang)), cands, QUOTA["a2_per_lang"]):
            r, d = c.split("\t")
            rows.append((f"a2{lang}-{int(r)}-{d}", f"Tranco ccTLD {lang}", f"https://{d}/", "", f"home lang={lang}"))
    return rows


# ---- A4 难题：五个来源各 40，不够 40 的全要 ----

def wpt_cases():
    """WPT html5lib .dat 文件里的每个 #data 用例：(文件相对路径, 序号)。"""
    out = []
    for p in sorted((DATA / "wpt-parsing").rglob("*.dat")):
        n = sum(1 for line in p.read_text(encoding="utf-8", errors="replace").splitlines() if line == "#data")
        out += [f"{p.relative_to(DATA).as_posix()}#{i}" for i in range(n)]
    return out


def a4():
    n = QUOTA["a4_per_source"]
    s = SEEDS["a4_hard"]
    rows = []
    enc = [p.relative_to(DATA).as_posix() for p in (DATA / "fresh/data/chardet-test-data").rglob("*.html")]
    rng = random.Random(s + 10)
    for i, x in enumerate(pick(s, enc, n)):
        kind = rng.choice(["head-trunc", "tail-trunc", "noise"])  # adversarial_encoding.py 的三种变形
        rows.append((f"a4enc-{i:02d}", "chardet-test-data HTML（编码陷阱）", x, "", f"{kind} seed={s + 100 + i}"))
    rows += [(f"a4wpt-{i:02d}", "WPT html5lib .dat 用例", x, "", "") for i, x in enumerate(pick(s + 1, wpt_cases(), n))]
    snaps = [p for d in ("snapshots", "snapshots-b", "crawl-pages", "crawl-pages-b") for p in (DATA / d).rglob("*.html")]
    big = [p.relative_to(DATA).as_posix() for p in snaps if p.stat().st_size > 5 * 2**20]
    rows += [(f"a4big-{i:02d}", "已抓快照中 > 5 MB 的页面", x, "", "") for i, x in enumerate(pick(s + 2, big, n))]
    blocked = []
    for d in ("c/c2/raw-blocked", "c/c2/raw-blocked2", "fix2/c2/raw-blocked3", "fix2/c2/raw-blocked4"):
        blocked += [f"tasks:{d}/{p.name}" for p in (TASKS / d).glob("*.html")]
    rows += [(f"a4blk-{i:02d}", "0.80 C2 抓到的拦截页、挑战页", x, "", "") for i, x in enumerate(pick(s + 3, blocked, n))]
    rng = random.Random(s + 4)
    rows += [(f"a4deep-{i:02d}", "生成的深层嵌套", f"generated:nest:{rng.randint(1000, 100000)}", "", "") for i in range(n)]
    return rows


# ---- B1 olmOCR 原生数字 PDF：全部，不抽 ----

def b1():
    root = DATA / "olmocr_1403/bench_data/pdfs"
    rows = []
    for cat in sorted(p.name for p in root.iterdir() if p.is_dir()):
        if cat.startswith("old_scans"):  # 扫描件不属于原生数字子集
            continue
        rows += [(f"b1-{cat}-{p.stem}", f"olmOCR-bench {cat}", p.relative_to(DATA).as_posix(), "", "") for p in sorted((root / cat).glob("*.pdf"))]
    return rows


# ---- B2 PDF 随机真实文件：arXiv 50、GOV.UK 50 ----

def b2_arxiv(n, seed):
    """按种子抽 2010-01..2026-09 的月份，每月取 arXiv API 里最早提交的 50 篇作候选。"""
    rng = random.Random(seed)
    months = sorted({(y, m) for y, m in ((rng.randint(2010, 2026), rng.randint(1, 12)) for _ in range(n)) if (y, m) <= (2026, 9)})
    cands = set()
    for y, m in months:
        q = urllib.parse.urlencode({"search_query": f"submittedDate:[{y}{m:02d}010000 TO {y}{m:02d}282359]",
                                    "start": 0, "max_results": 50, "sortBy": "submittedDate", "sortOrder": "ascending"})
        xml = get(f"https://export.arxiv.org/api/query?{q}", min_gap=3.0)
        cands |= {seg.split("</id>")[0].rsplit("/abs/", 1)[-1] for seg in xml.split("<id>")[2:]}
    return [(f"b2arxiv-{x.replace('/', '_')}", "arXiv", f"https://arxiv.org/pdf/{x}", f"https://arxiv.org/abs/{x}", "")
            for x in pick(seed, cands, n)]


def b2_gov(n, seed):
    """GOV.UK 搜索 API 的“研究与统计”类，按种子取随机偏移处各 20 条作候选；PDF 附件在 fetch.py 里用内容 API 取。"""
    total = get("https://www.gov.uk/api/search.json?filter_content_purpose_supergroup=research_and_statistics&count=0", True)["total"]
    rng = random.Random(seed)
    cands = set()
    for _ in range(n):
        q = urllib.parse.urlencode({"filter_content_purpose_supergroup": "research_and_statistics", "count": 20,
                                    "start": rng.randrange(max(1, total - 20)), "fields": "link", "order": "-public_timestamp"})
        cands |= {r["link"] for r in get(f"https://www.gov.uk/api/search.json?{q}", True)["results"]}
    return [(sha_id("b2gov", x), "GOV.UK 研究与统计", f"https://www.gov.uk/api/content{x}", f"https://www.gov.uk{x}", "resolve:govuk")
            for x in pick(seed, cands, n)]


def b2():
    q = QUOTA["b2"]
    # SEC EDGAR 去掉：SEC 要求 UA 带联系邮箱（RULES 第三节）
    return b2_arxiv(q["arxiv"], SEEDS["b2_arxiv"]) + b2_gov(q["govuk"], SEEDS["b2_gov"])


# ---- B3 PDF 难题：本地 pdf.js 测试集 1010 份，按 test_manifest 分 5 层，每层按种子抽 20 ----

def b3_stratum(path, entries):
    """按可客观判断的特征归层，依次判断、取第一个满足的。"""
    b = path.read_bytes()
    if any("password" in e for e in entries) or b"/Encrypt" in b:
        return "encrypted"
    if len(b) > 2**20:
        return "large"
    if any(k in e for e in entries for k in ("forms", "annotations", "annotationStorage", "enableXfa", "print")):
        return "forms"
    if not entries or any(e.get("type") == "load" for e in entries):
        return "load"  # 清单里只测能否加载的，或不在清单里的回归样本（多为损坏、畸形文件）
    return "other"


def b3():
    lst = (TASKS / "a0/list-pdfjs.txt").read_text().split()
    manifest = json.loads((DATA / "pdfjs/test/test_manifest.json").read_text())
    by_file = {}
    for e in manifest:
        by_file.setdefault(Path(e.get("file", "")).name, []).append(e)
    strata = {}
    for x in lst:
        p = Path(x).resolve()
        strata.setdefault(b3_stratum(p, by_file.get(p.name, [])), []).append(p.relative_to(DATA.resolve()).as_posix())
    per = QUOTA["b3_total"] // len(strata)
    rows, left = [], []
    for i, name in enumerate(sorted(strata)):
        chosen = pick(SEEDS["b3_hard"] + i, strata[name], per)
        rows += [(f"b3-{Path(x).stem}", f"pdf.js test/pdfs（{name}）", x, "", f"stratum={name}") for x in chosen]
        left += [(name, x) for x in strata[name] if x not in chosen]
    # 某层不够 per 份时，空出来的名额从各层剩下的文件里按种子补
    for name, x in pick(SEEDS["b3_hard"] + 99, left, QUOTA["b3_total"] - len(rows)):
        rows.append((f"b3-{Path(x).stem}", f"pdf.js test/pdfs（{name}）", x, "", f"stratum={name} fill"))
    return rows


# ---- B4 Office 与电子书：修复 3 的开发集 + 留出集，全部 ----

def b4():
    rows = []
    for split in ("dev", "holdout"):
        for p in sorted((OFFICE / split).rglob("*")):
            if p.is_file() and p.suffix.lower() in (".docx", ".xlsx", ".pptx", ".epub", ".csv"):
                rows.append((f"b4-{split}-{p.suffix[1:].lower()}-{p.stem}"[:120], f"修复 3 {split}",
                             f"office:{split}/{p.relative_to(OFFICE / split).as_posix()}", "", ""))
    return rows


SUBCLASSES = {"a1": a1, "a2": a2, "a4": a4, "b1": b1, "b2": b2, "b3": b3, "b4": b4}


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
        assert len({r[0] for r in rows}) == len(rows), f"{name}: id 重复"
        with open(out / f"{name}.tsv", "w", encoding="utf-8") as f:
            f.write("id\tsource\tlocation\torig_url\tnote\tmodel_subset\n")
            for id_, src, loc, url, note in rows:
                sub = int(in_subset(SEEDS["model_subset"], id_, QUOTA["model_fraction"]))
                f.write(f"{id_}\t{src}\t{loc}\t{url}\t{note}\t{sub}\n")
        print(f"{name}: {len(rows)} 份 → {out / f'{name}.tsv'}", flush=True)


if __name__ == "__main__":
    main()

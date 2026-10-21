"""编码鲁棒性测试：网站返回的原始字节（不一定是 UTF-8）交给各工具，看它们能不能正确读出来。

    python3 -I bench/fresh/encoding.py pick <随机语料目录> <输出清单>   挑出不是 UTF-8 的页面，清单指向原始字节
    python3 -I bench/fresh/encoding.py score <原始字节结果目录> <UTF-8 结果目录> <工具>...

评判时每个工具只和它自己比：同一页面，读原始字节得到的输出和读 UTF-8 副本得到的输出（随机语料对比里的那份）
几乎一样（字符级相似度 ≥ 0.9）就算正确读出；输出为空算失败，其余算乱码。这样只看编码处理，不掺入抽取能力的差别。
"""
import difflib
import json
import re
import sys
from pathlib import Path

META = re.compile(rb'<meta[^>]+charset\s*=\s*["\']?\s*([A-Za-z0-9_.:-]+)', re.I)


def pick(corpus: Path, out: Path) -> None:
    n = 0
    with out.open("w") as f:
        for line in (corpus / "manifest.jsonl").open():
            r = json.loads(line)
            raw = (corpus / "pages" / f"{r['id']}.html").read_bytes()
            m = META.search(raw[:4096])
            declared = m.group(1).decode("ascii", "ignore").lower() if m else ""
            try:
                raw.decode("utf-8")
                valid = True
            except UnicodeDecodeError:
                valid = False
            if not valid or (declared and declared not in ("utf-8", "utf8")):
                n += 1
                f.write(json.dumps({"id": r["id"], "path": str(corpus / "pages" / f"{r['id']}.html"),
                                    "url": r["final_url"], "base_url": r["final_url"], "declared": declared}) + "\n")
    print(f"{n} pages are not UTF-8", file=sys.stderr)


def load(path: Path) -> dict:
    if not path.exists():
        return {}
    return {r["id"]: r.get("markdown") or "" for r in map(json.loads, path.open()) if r}


def score(raw_dir: Path, utf8_dir: Path, tools: list) -> None:
    print(f"{'工具':24} {'正确读出↑':>8} {'乱码↓':>6} {'失败或空↓':>8} {'页数':>5}")
    for t in tools:
        sub = "crawl4ai/crawl4ai-raw.jsonl" if t == "crawl4ai" else f"{t}.jsonl"
        raw = load(raw_dir / sub)
        ref = load(utf8_dir / (f"random-{sub}" if t != "crawl4ai" else "random-crawl4ai/crawl4ai-raw.jsonl"))
        ok = bad = empty = 0
        for pid, text in raw.items():
            want = ref.get(pid, "")
            if not text.strip():
                empty += 1
            elif difflib.SequenceMatcher(None, text[:20000], want[:20000], autojunk=False).ratio() >= 0.9:
                ok += 1
            else:
                bad += 1
        print(f"{t:24} {ok:8} {bad:6} {empty:8} {len(raw):5}")


if __name__ == "__main__":
    if sys.argv[1] == "pick":
        pick(Path(sys.argv[2]), Path(sys.argv[3]))
    else:
        score(Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4:])

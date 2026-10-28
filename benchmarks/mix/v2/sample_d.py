"""第 2 版 D（定向爬取）的学校抽样，规则见 RULES.md 第二节。只用标准库。

    python3 -I benchmarks/mix/v2/sample_d.py

- 名单：Hipo university-domains-list 在 seeds.toml 记下的提交上的 world_universities_and_domains.json（记下 SHA-256）。
- 去掉 pagesift 开发时用过的 69 所（benchmarks/history/0.80.9/sites.txt）：主机名去掉 www. 后相同，或名单里的域名
  是开发集主机的后缀，都算同一所。
- 每所取名单里的第一个 web_pages 作首页；按 (名称, 首页) 排序后用 d_universities 种子抽 30 所。首页打不开的到爬的时候
  照实记“不可用”，不换。输出 v2/samples/d.tsv。
"""
import hashlib
import json
import random
import tomllib
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
CONF = tomllib.loads((HERE / "seeds.toml").read_text())
UA = "pagesift-mix-bench/0.80 (+https://github.com/YOLOW92/pagesift)"


def host(url):
    h = url.split("//", 1)[-1].split("/", 1)[0].lower()
    return h[4:] if h.startswith("www.") else h


def main():
    d = CONF["d"]
    raw_url = d["source"].replace("github.com", "raw.githubusercontent.com") + f"/{d['source_commit']}/world_universities_and_domains.json"
    body = urllib.request.urlopen(urllib.request.Request(raw_url, headers={"User-Agent": UA}), timeout=60).read()
    sha = hashlib.sha256(body).hexdigest()
    unis = json.loads(body)
    dev = {host(line.split()[1]) for line in (REPO / d["exclude"]).read_text().splitlines() if line.strip()}
    cands = []
    for u in unis:
        pages = u.get("web_pages") or []
        if not pages:
            continue
        home = pages[0] if "://" in pages[0] else f"https://{pages[0]}"
        hs = {host(home)} | {x.lower() for x in u.get("domains") or []}
        if any(h == x or x.endswith("." + h) or h.endswith("." + x) for h in hs for x in dev):
            continue
        cands.append((u["name"], home, u.get("country", ""), u.get("alpha_two_code", "")))
    cands = sorted(set(cands))
    idx = sorted(random.Random(CONF["seeds"]["d_universities"]).sample(range(len(cands)), CONF["quota"]["d_universities"]))
    rows = [cands[i] for i in idx]
    (HERE / "samples").mkdir(exist_ok=True)
    out = ["id\tname\thome\tcountry\tcode\tsource_sha256"]
    for n, (name, home, country, code) in enumerate(rows):
        out.append(f"d{n:02d}\t{name}\t{home}\t{country}\t{code}\t{sha}")
    (HERE / "samples/d.tsv").write_text("\n".join(out) + "\n", encoding="utf-8")
    print(f"名单 {len(unis)} 所，去掉开发集后 {len(cands)} 所，抽 {len(rows)} 所；文件 SHA-256 {sha}")
    for r in rows:
        print("  ", r[0], r[1], r[2])


if __name__ == "__main__":
    main()

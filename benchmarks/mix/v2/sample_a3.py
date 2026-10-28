"""第 2 版 A3（需要 JS 渲染的网页）抽样，规则见 RULES.md 第二节。只用标准库，抓取复用第 1 版 fetch.py 的 Fetcher
（遵守 robots.txt、同主机间隔 1 秒、UA 写仓库地址）。

    python3 -I benchmarks/mix/v2/sample_a3.py [--out $MIX/inputs] [--threads 32]

- 候选：Tranco 前 100 万（第 1 版同一份 top-1m.csv），按第 1 版 A2 的 5 层；每层的站点按 a3_tranco 种子打乱
  （去掉第 1 版 A2 已抽到的站点），按这个顺序静态抓首页、判断合格，每层取前 20 份合格的。
- 合格只看原始 HTML：去掉 script/style/noscript/template 后的可见文字 < 500 字，且外部脚本 ≥ 3 个或有常见前端框架
  的挂载点（id 为 root、app、__next、__nuxt、svelte）。不看任何参评工具的输出。
- 输出 v2/samples/a3.tsv（入选）和 a3.candidates.tsv（每个试过的候选：状态、可见字数、脚本数、挂载点、原因），
  原始 HTML 存 <out>/a3/<id>.html。合格页是实时抓的对象，这里存的静态 HTML 只作记录。
"""
import argparse
import random
import sys
import tomllib
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from html.parser import HTMLParser
from pathlib import Path

HERE = Path(__file__).resolve().parent
MIXDIR = HERE.parent
sys.path.insert(0, str(MIXDIR))
import fetch  # noqa: E402  第 1 版的 Fetcher

CONF = tomllib.loads((HERE / "seeds.toml").read_text())
V1 = tomllib.loads((MIXDIR / "seeds.toml").read_text())
DATA = MIXDIR.parent / "data"
A3 = CONF["a3"]


class Probe(HTMLParser):
    HIDDEN = {"script", "style", "noscript", "template"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.depth = 0
        self.text = []
        self.scripts = 0
        self.mounts = set()

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == "script" and a.get("src"):
            self.scripts += 1
        if a.get("id") in A3["mount_ids"]:
            self.mounts.add(a["id"])
        if tag in self.HIDDEN:
            self.depth += 1

    def handle_endtag(self, tag):
        if tag in self.HIDDEN and self.depth:
            self.depth -= 1

    def handle_data(self, data):
        if not self.depth:
            self.text.append(data)


def probe(body):
    p = Probe()
    try:
        p.feed(body.decode("utf-8", "replace"))
        p.close()
    except Exception:  # noqa: BLE001  坏 HTML 也照样数到这里为止
        pass
    visible = len(" ".join("".join(p.text).split()))
    ok = visible < A3["max_visible_chars"] and (p.scripts >= A3["min_external_scripts"] or bool(p.mounts))
    return ok, visible, p.scripts, ",".join(sorted(p.mounts))


def try_one(f, out, tier, rank, domain):
    id_ = f"a3-{rank}-{domain}"
    url = f"https://{domain}/"
    try:
        final, status, headers, body = f.get(url, 15 << 20)
    except Exception as e:  # noqa: BLE001  抓不到照实记
        return dict(id=id_, tier=tier, url=url, final="", status="unavailable", visible="", scripts="", mounts="", reason=str(e)[:200])
    ctype = next((v for k, v in headers if k.lower() == "content-type"), "")
    if "html" not in ctype.lower():
        return dict(id=id_, tier=tier, url=url, final=final, status="not-html", visible="", scripts="", mounts="", reason=ctype[:80])
    ok, visible, scripts, mounts = probe(body)
    if ok:
        (out / "a3").mkdir(parents=True, exist_ok=True)
        (out / "a3" / f"{id_}.html").write_bytes(body)
    return dict(id=id_, tier=tier, url=url, final=final, status="qualified" if ok else "static", visible=visible, scripts=scripts,
                mounts=mounts, reason="")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=Path.home() / "mix-tools/inputs")
    ap.add_argument("--threads", type=int, default=32)
    ap.add_argument("--max-tries", type=int, default=1500, help="每层最多试多少个候选（到了还不够 20 份就照实记）")
    a = ap.parse_args()
    taken = {l.split("\t")[2].split("/")[2] for l in (MIXDIR / "samples/a2.tsv").read_text(encoding="utf-8").splitlines()[1:] if l}
    ranks = []
    with open(DATA / "top-1m.csv", encoding="utf-8") as fh:
        for line in fh:
            r, d = line.strip().split(",", 1)
            ranks.append((int(r), d))
    f = fetch.Fetcher()
    chosen, tried = [], []
    for ti, (lo, hi) in enumerate(V1["a2"]["tiers"]):
        tier = f"Tranco {lo}-{hi - 1}"
        cands = sorted((r, d) for r, d in ranks if lo <= r < hi and d not in taken)
        random.Random(CONF["seeds"]["a3_tranco"] + ti).shuffle(cands)
        got, pos = [], 0
        with ThreadPoolExecutor(a.threads) as ex:
            while len(got) < CONF["quota"]["a3_per_tier"] and pos < min(len(cands), a.max_tries):
                batch = cands[pos:pos + a.threads]
                pos += len(batch)
                for res in ex.map(lambda rd: try_one(f, a.out, tier, *rd), batch):  # 保持候选顺序
                    tried.append(res)
                    if res["status"] == "qualified" and len(got) < CONF["quota"]["a3_per_tier"]:
                        got.append(res)
        print(f"{tier}: 试了 {pos} 个，合格 {len(got)} 份", flush=True)
        chosen += got
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    cols = ["id", "tier", "url", "final", "status", "visible", "scripts", "mounts", "reason"]
    (HERE / "samples").mkdir(exist_ok=True)
    (HERE / "samples/a3.candidates.tsv").write_text("\t".join(cols) + "\n" + "".join("\t".join(str(r[c]) for c in cols) + "\n" for r in tried), encoding="utf-8")
    (HERE / "samples/a3.tsv").write_text("\t".join(cols + ["sampled_at"]) + "\n" + "".join("\t".join([str(r[c]) for c in cols] + [now]) + "\n" for r in chosen), encoding="utf-8")
    print(f"A3 入选 {len(chosen)} 份", flush=True)


if __name__ == "__main__":
    main()

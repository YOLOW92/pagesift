"""正文提取 A1 四套集（AEB、Firecrawl、WCXB、WebMainBench 各 150 页）：用给定二进制逐页 `convert`，
写成 benchmarks/mix/score_quality.py 认的目录结构。只用标准库。

    python3 -I run_a1.py <pagesift 二进制> <输出根目录> <工具名> [--jobs N]

输入与 0.81 质量轮相同（$MIX_TOOLS/inputs/a1.manifest，默认 ~/mix-tools）；参数也相同：
`pagesift convert <文件> [--base-url <原始 URL>]`，PAGESIFT_CONFIG 置空（全用默认）。
写 <输出根目录>/a1/<工具名>/results.jsonl 与 <id>.md。质量评测不计时，所以直接并发跑。
"""
import argparse
import json
import os
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

MIX = Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools"))
SAMPLES = Path(__file__).resolve().parents[3] / "mix/samples/a1.tsv"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bin")
    ap.add_argument("root")
    ap.add_argument("tool")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    a = ap.parse_args()
    out = Path(a.root) / "a1" / a.tool
    out.mkdir(parents=True, exist_ok=True)
    lines = SAMPLES.read_text(encoding="utf-8").splitlines()
    cols = lines[0].split("\t")
    url = {r["id"]: r.get("orig_url", "") for r in (dict(zip(cols, l.split("\t"))) for l in lines[1:] if l)}
    man = [l.split("\t") for l in (MIX / "inputs/a1.manifest").read_text(encoding="utf-8").splitlines() if l]
    env = {**os.environ, "PAGESIFT_CONFIG": ""}

    def one(row):
        id_, path = row[0], row[1]
        c = [a.bin, "convert", path]
        if url.get(id_):
            c += ["--base-url", url[id_]]
        r = subprocess.run(c, capture_output=True, env=env)
        ok = r.returncode == 0
        if ok:
            (out / f"{id_}.md").write_bytes(r.stdout)
        st = "ok" if ok and r.stdout else ("empty" if ok else "error")
        return {"tool": a.tool, "id": id_, "input": path, "status": st, "bytes": len(r.stdout) if ok else 0,
                "error": None if ok else r.stderr.decode("utf-8", "replace")[-500:]}

    with ThreadPoolExecutor(a.jobs) as ex:
        rows = list(ex.map(one, man))
    (out / "results.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
    print(f"{a.tool}: {sum(r['status'] == 'ok' for r in rows)}/{len(rows)} ok")


if __name__ == "__main__":
    main()

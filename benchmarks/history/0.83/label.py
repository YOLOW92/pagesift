#!/usr/bin/env python3
"""0.83 质量分的输入与标签。

  label.py prepare <repo> <run>            # 解压四个数据集的 HTML，写 <run>/manifest.jsonl
  label.py recall  <repo> <run>            # 统一口径的词召回，写 <run>/recall.jsonl

四个集用同一种召回（见 recall()）。各集自己的官方口径（AEB 的 shingle、Firecrawl 的短语命中）
不一致：第一次试跑时 Firecrawl 22% 的页短语召回为 0、负例率是其他集的四倍多，数据集成了特征看不到的隐变量。
"""
import csv
import gzip
import importlib.util
import json
import sys
from pathlib import Path


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def prepare(repo: Path, run: Path):
    data = repo / "benchmarks/data"
    html = run / "html"
    html.mkdir(parents=True, exist_ok=True)
    rows = []
    aeb_truth = json.loads((data / "fresh/data/aeb/ground-truth.json").read_text())
    for gz in sorted((data / "fresh/data/aeb/html").glob("*.html.gz")):
        key = gz.name.removesuffix(".html.gz")
        p = html / f"aeb-{key}.html"
        p.write_bytes(gzip.decompress(gz.read_bytes()))
        rows.append({"id": f"aeb-{key}", "set": "aeb", "path": str(p), "url": aeb_truth.get(key, {}).get("url")})
    gt_dir = data / "fresh/data/wcxb/test/ground-truth"
    for gz in sorted((data / "fresh/data/wcxb/test/html").glob("*.html.gz")):
        key = gz.name.removesuffix(".html.gz")
        if not (gt_dir / f"{key}.json").exists():
            continue
        gt = json.loads((gt_dir / f"{key}.json").read_text())
        p = html / f"wcxb-{key}.html"
        p.write_bytes(gzip.decompress(gz.read_bytes()))
        rows.append({"id": f"wcxb-{key}", "set": "wcxb", "path": str(p), "url": gt.get("url")})
    with open(data / "p4_2_webmainbench/WebMainBench_545.jsonl", encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            p = html / f"wmb-{r['track_id']}.html"
            p.write_text(r["html"], encoding="utf-8")
            rows.append({"id": f"wmb-{r['track_id']}", "set": "wmb", "path": str(p), "url": r.get("url")})
    with open(data / "inputs/manifest.jsonl", encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            p = data / r["path"].removeprefix("bench/data/")
            if p.exists():
                rows.append({"id": f"fc-{r['id']}", "set": "fc", "path": str(p), "url": r.get("url")})
    with open(run / "manifest.jsonl", "w", encoding="utf-8") as g:
        for r in rows:
            g.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"prepare: {len(rows)} pages", file=sys.stderr)


def recall(repo: Path, run: Path):
    """四个集统一口径：输出经 wcxb.plain 转纯文本，与标准答案算 wcxb.word_f1 的词召回
    （小写 \\w+ 分词，多重集重叠 / 答案词数）。没有标准答案文字的页不算。"""
    data = repo / "benchmarks/data"
    wcxb = load(repo / "benchmarks/common/fresh/wcxb.py", "mix_wcxb")
    wmb = load(repo / "benchmarks/common/fresh/webmain.py", "mix_webmain")
    refs = {}
    aeb_truth = json.loads((data / "fresh/data/aeb/ground-truth.json").read_text())
    for key, v in aeb_truth.items():
        refs[f"aeb-{key}"] = v.get("articleBody", "")
    for f in (data / "fresh/data/wcxb/test/ground-truth").glob("*.json"):
        refs[f"wcxb-{f.stem}"] = json.loads(f.read_text())["ground_truth"]["main_content"]
    with open(data / "p4_2_webmainbench/WebMainBench_545.jsonl", encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            refs[f"wmb-{r['track_id']}"] = wmb.plain(r.get("convert_main_content") or r.get("groundtruth_content") or "")
    csv.field_size_limit(1 << 30)
    with open(data / "1-0-0.csv", encoding="utf-8", newline="") as f:
        for r in csv.DictReader(f):
            refs[f"fc-{r['id']}"] = r.get("truth_text") or ""
    out = []
    for line in open(run / "features.jsonl", encoding="utf-8"):
        r = json.loads(line)
        ref = refs.get(r["id"], "")
        if not wcxb.tokenize(ref):
            continue
        out.append({"id": r["id"], "recall": wcxb.word_f1(wcxb.plain(r["markdown"] or ""), ref)[1]})
    with open(run / "recall.jsonl", "w", encoding="utf-8") as g:
        for r in out:
            g.write(json.dumps(r) + "\n")
    print(f"recall: {len(out)} pages", file=sys.stderr)


if __name__ == "__main__":
    cmd, repo, run = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
    if cmd == "prepare":
        prepare(repo, run)
    else:
        recall(repo, run)

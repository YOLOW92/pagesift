"""把本地来源的样本（A1、A4、B1、B3、B4）变成工具的输入文件。只用标准库，不联网。

    python3 -I bench/mix/materialize.py <a1|a4|b1|b3|b4|all> [--samples bench/mix/samples] [--out $MIX/inputs]

- 原样能用的文件建符号链接（<out>/<子类>/<id>.<扩展名>）；要变换的（WCXB 的 .gz、WebMainBench 的 jsonl、A4 的
  编码变形、WPT 用例、深层嵌套）写成新文件。变换都由 sample.py 记下的种子决定，重跑结果逐字节相同。
- 输出 samples/<子类>.inputs.tsv：id、本地路径（相对 --out）、字节数、SHA-256、原始 URL；另写 <out>/<子类>.manifest
  给 driver.py。
"""
import argparse
import gzip
import hashlib
import json
import os
import random
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONF = tomllib.loads((HERE / "seeds.toml").read_text())
CHECKOUT = Path(os.environ.get("PAGESIFT_CHECKOUT", Path.home() / "pagesift"))
DATA = Path(os.environ.get("MIX_DATA", CHECKOUT / "bench/data"))
OFFICE = Path(os.environ.get("MIX_OFFICE", CHECKOUT / "tasks/0.80/fix3/office"))
TASKS = Path(os.environ.get("MIX_TASKS", CHECKOUT / "tasks/0.80"))


def source(loc):
    if loc.startswith("tasks:"):
        return TASKS / loc[len("tasks:"):]
    if loc.startswith("office:"):
        return OFFICE / loc[len("office:"):]
    return DATA / loc


# ---- 变换 ----

def adversarial(b, kind, seed):
    """与 bench/fresh/adversarial_encoding.py 的三种变形相同，随机数换成固定种子。"""
    rng = random.Random(seed)
    if kind == "head-trunc":  # 截掉开头 1–3 字节（破坏 BOM 和多字节首字节）
        return b[min(rng.randint(1, 3), len(b)):]
    if kind == "tail-trunc":  # 只留前 128 字节（多半截在多字节字符中间）
        return b[: min(128, len(b))]
    if kind == "noise":  # 约 1% 的字节换成非法值，最多 20 处
        arr = bytearray(b)
        for _ in range(min(max(1, len(arr) // 100), 20)):
            arr[rng.randint(0, len(arr) - 1)] = rng.choice([0x00, 0xFF, 0x80, 0xFE])
        return bytes(arr)
    raise ValueError(kind)


def wpt_case(loc):
    """<文件>.dat#<序号>：第 n 个用例 #data 段的原文，原样当整页。"""
    path, n = loc.rsplit("#", 1)
    lines = (DATA / path).read_text(encoding="utf-8", errors="surrogateescape").split("\n")
    starts = [i for i, l in enumerate(lines) if l == "#data"]
    i = starts[int(n)] + 1
    j = i
    while j < len(lines) and not (lines[j].startswith("#") and lines[j] in ("#errors", "#new-errors", "#document-fragment", "#script-off", "#script-on", "#document")):
        j += 1
    return "\n".join(lines[i:j]).encode("utf-8", "surrogateescape")


def deep_nest(depth):
    """depth 层嵌套的 div，最里面一段正文。"""
    return (b"<!DOCTYPE html><html><head><title>nest</title></head><body>" + b"<div>" * depth
            + b"<p>deep text</p>" + b"</div>" * depth + b"</body></html>")


def wmb_html(track_id):
    with open(DATA / "p4_2_webmainbench/webmainbench.jsonl", encoding="utf-8") as f:
        for line in f:
            if track_id in line:
                r = json.loads(line)
                if r["track_id"] == track_id:
                    return r["html"].encode("utf-8")
    raise KeyError(track_id)


def build(row, sub, out):
    """返回本地路径（新写的文件或符号链接）。"""
    id_, loc, note = row["id"], row["location"], row["note"]
    d = out / sub
    if loc.startswith("generated:nest:"):
        p = d / f"{id_}.html"
        p.write_bytes(deep_nest(int(loc.rsplit(":", 1)[1])))
        return p
    if ".dat#" in loc:
        p = d / f"{id_}.html"
        p.write_bytes(wpt_case(loc))
        return p
    if loc.startswith("p4_2_webmainbench/webmainbench.jsonl#"):
        p = d / f"{id_}.html"
        p.write_bytes(wmb_html(loc.split("#", 1)[1]))
        return p
    src = source(loc)
    if src.suffix == ".gz":
        p = d / f"{id_}.html"
        p.write_bytes(gzip.decompress(src.read_bytes()))
        return p
    if note.split(" ")[0] in ("head-trunc", "tail-trunc", "noise"):
        kind, seed = note.split(" ")
        p = d / f"{id_}.html"
        p.write_bytes(adversarial(src.read_bytes(), kind, int(seed.split("=")[1])))
        return p
    p = d / f"{id_}{src.suffix.lower()}"
    if p.is_symlink() or p.exists():
        p.unlink()
    p.symlink_to(src.resolve())
    return p


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("which", choices=["a1", "a4", "b1", "b3", "b4", "all"])
    ap.add_argument("--samples", default=str(HERE / "samples"))
    ap.add_argument("--out", default=str(Path.home() / "mix-tools/inputs"))
    a = ap.parse_args()
    if not CONF.get("frozen"):
        sys.exit("规则还没冻结（seeds.toml: frozen = false），不生成。")
    out = Path(a.out)
    for sub in (["a1", "a4", "b1", "b3", "b4"] if a.which == "all" else [a.which]):
        lines = (Path(a.samples) / f"{sub}.tsv").read_text(encoding="utf-8").splitlines()
        cols = lines[0].split("\t")
        rows = [dict(zip(cols, l.split("\t"))) for l in lines[1:] if l]
        (out / sub).mkdir(parents=True, exist_ok=True)
        with open(Path(a.samples) / f"{sub}.inputs.tsv", "w", encoding="utf-8") as fi, open(out / f"{sub}.manifest", "w", encoding="utf-8") as fm:
            fi.write("id\tpath\tbytes\tsha256\torig_url\n")
            missing = 0
            for row in rows:
                try:
                    p = build(row, sub, out)
                except (FileNotFoundError, KeyError, IndexError) as e:
                    missing += 1
                    fi.write(f"{row['id']}\t\t\t\t{row['orig_url']}\t# 缺：{e}\n")
                    continue
                b = p.read_bytes()
                fi.write(f"{row['id']}\t{p.relative_to(out).as_posix()}\t{len(b)}\t{hashlib.sha256(b).hexdigest()}\t{row['orig_url']}\n")
                fm.write(f"{row['id']}\t{p}\t{row['orig_url']}\n")
        print(f"{sub}: {len(rows) - missing}/{len(rows)} 份输入 → {out / sub}", flush=True)


if __name__ == "__main__":
    main()

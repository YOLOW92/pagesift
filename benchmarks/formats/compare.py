"""新格式的对比：同一批文件上跑 pagesift 和 xberg，和标准答案算词袋 F1（越高越好）。

    python3 -I benchmarks/formats/compare.py \
        --documents ~/mix-tools/inputs-110/td --fixtures <xberg>/tools/benchmark-harness/fixtures \
        --pagesift <二进制> --xberg-python ~/mix-tools/py/xberg/bin/python --out <结果目录>

文件和标准答案来自 xberg-io/test_documents，那个仓库没有声明许可证，所以只在本地读取，不放进本仓库，
结果里也只有分数和文件名。F1 按小写的词计（Unicode 字母数字串），词序和 Markdown 记号不影响分数：
这里比的是内容有没有取全，结构另算。转不了的文件 F1 记 0，并单独列出。
"""
import argparse
import collections
import json
import re
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

WORD = re.compile(r"\w+", re.UNICODE)
XBERG = r"""
import asyncio, json, sys
from xberg import ExtractInput, ExtractionConfig, extract
cfg = ExtractionConfig(output_format="markdown", use_cache=False)
loop = asyncio.new_event_loop()
for line in sys.stdin:
    job = json.loads(line)
    try:
        out = loop.run_until_complete(extract(ExtractInput(kind="uri", uri=job["path"]), cfg))
        text = "\n\n".join(r.content or "" for r in out.results)
        print(json.dumps({"ok": True, "text": text}), flush=True)
    except BaseException as e:
        print(json.dumps({"ok": False, "error": f"{type(e).__name__}: {e}"[:300]}), flush=True)
"""


def f1(text, truth):
    a = collections.Counter(w.lower() for w in WORD.findall(text))
    b = collections.Counter(w.lower() for w in WORD.findall(truth))
    if not b:
        return None
    common = sum((a & b).values())
    if not common:
        return 0.0
    p, r = common / sum(a.values()), common / sum(b.values())
    return 2 * p * r / (p + r)


def pagesift(binary, path):
    try:
        r = subprocess.run([binary, "scrape", str(path)], capture_output=True, timeout=120)
    except subprocess.TimeoutExpired:
        return None, "timeout"
    if r.returncode != 0 or r.stdout[:20].startswith(b"UNSUPPORTED"):
        msg = (r.stdout + r.stderr).decode("utf-8", "replace")[:200]
        return None, msg
    return r.stdout.decode("utf-8", "replace"), None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--documents", required=True, type=Path)
    ap.add_argument("--fixtures", required=True, type=Path)
    ap.add_argument("--pagesift", required=True)
    ap.add_argument("--xberg-python", required=True)
    ap.add_argument("--out", required=True, type=Path)
    a = ap.parse_args()
    jobs = []
    for fx in sorted(a.fixtures.glob("*.json")):
        try:
            d = json.loads(fx.read_text(encoding="utf-8"))
        except ValueError:
            continue
        truth_file = (d.get("ground_truth") or {}).get("text_file")
        if not truth_file or "document" not in d:
            continue
        rel = d["document"].split("test_documents/", 1)[-1]
        truth_rel = truth_file.split("test_documents/", 1)[-1]
        doc, truth = a.documents / rel, a.documents / truth_rel
        # 二进制文件不在那个仓库里，取不到的跳过
        if doc.exists() and truth.exists():
            jobs.append((d.get("file_type") or doc.suffix.lstrip("."), doc, truth))
    with tempfile.TemporaryDirectory() as tmp:
        script = Path(tmp) / "xberg_worker.py"
        script.write_text(XBERG, encoding="utf-8")
        worker = subprocess.Popen(
            [a.xberg_python, "-I", str(script)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            cwd=tmp,
        )
        rows = []
        for kind, doc, truth in jobs:
            answer = truth.read_text(encoding="utf-8", errors="replace")
            text, error = pagesift(a.pagesift, doc)
            worker.stdin.write(json.dumps({"path": str(doc)}) + "\n")
            worker.stdin.flush()
            x = json.loads(worker.stdout.readline())
            ps = 0.0 if text is None else f1(text, answer)
            xb = f1(x.get("text", ""), answer) if x["ok"] else 0.0
            if ps is None or xb is None:
                continue
            rows.append(
                {
                    "type": kind,
                    "file": str(doc.relative_to(a.documents)),
                    "pagesift": ps,
                    "xberg": xb,
                    "pagesift_error": error,
                    "xberg_error": x.get("error"),
                }
            )
        worker.stdin.close()
        worker.wait(timeout=30)
    groups = collections.defaultdict(list)
    for r in rows:
        groups[r["type"]].append(r)
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "formats.json").write_text(json.dumps(rows, ensure_ascii=False, indent=1), encoding="utf-8")
    lines = ["| 类型 | 份数 | pagesift F1 ↑ | xberg F1 ↑ | pagesift 转不了 | xberg 转不了 |", "|---|---:|---:|---:|---:|---:|"]
    for kind, group in sorted(groups.items()):
        lines.append(
            f"| {kind} | {len(group)} | {statistics.mean(r['pagesift'] for r in group):.3f} | "
            f"{statistics.mean(r['xberg'] for r in group):.3f} | "
            f"{sum(1 for r in group if r['pagesift_error'])} | {sum(1 for r in group if r['xberg_error'])} |"
        )
    lines.append(
        f"| **全部** | {len(rows)} | {statistics.mean(r['pagesift'] for r in rows):.3f} | "
        f"{statistics.mean(r['xberg'] for r in rows):.3f} | "
        f"{sum(1 for r in rows if r['pagesift_error'])} | {sum(1 for r in rows if r['xberg_error'])} |"
    )
    report = "\n".join(lines) + "\n"
    (a.out / "formats.md").write_text(report, encoding="utf-8")
    print(report)
    worst = sorted(rows, key=lambda r: r["pagesift"] - r["xberg"])[:15]
    for r in worst:
        print(f"{r['pagesift'] - r['xberg']:+.3f} {r['pagesift']:.3f}/{r['xberg']:.3f} {r['file']} {(r['pagesift_error'] or '')[:70]}", file=sys.stderr)


if __name__ == "__main__":
    main()

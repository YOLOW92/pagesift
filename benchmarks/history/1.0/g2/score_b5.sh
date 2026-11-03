#!/usr/bin/env bash
# G2：B5（olmOCR-bench 的 old_scans、old_scans_math，扫描页）上带 OCR 的 pagesift 得分。
#   [PDF_OCR_FORCE=1] benchmarks/history/1.0/g2/score_b5.sh <输出目录> [检测模型目录 识别模型目录 [版面模型目录 公式模型目录]] | none
# 模型目录缺省是 ~/.cache/pagesift/models 下的 PP-OCRv6 small；给 none 时不开 OCR（对照）。
# 分数口径同 benchmarks/mix/score_quality.py 的 b5：各类别通过率的平均（含 baseline 类，官方口径）。
set -euo pipefail
cd "$(dirname "$0")/../../../.."
out=${1:?输出目录}
m=${XDG_CACHE_HOME:-$HOME/.cache}/pagesift/models
det=${2:-$m/PP-OCRv6_small_det_safetensors}
rec=${3:-$m/PP-OCRv6_small_rec_safetensors}
math=("${@:4:2}")
data=benchmarks/data/olmocr_1403/bench_data
bench=benchmarks/data/fresh/tools/olmocr-bench
py=$PWD/benchmarks/data/fresh/tools/py/bin/python
mkdir -p "$out/pdfs" "$out/data" "$out/cand"
out=$(cd "$out" && pwd)
for s in old_scans old_scans_math; do
  ln -sfn "$PWD/$data/pdfs/$s" "$out/pdfs/$s"
  ln -sfn "$PWD/$data/$s.jsonl" "$out/data/$s.jsonl"
done
if [ "$det" = none ]; then
  cargo build --profile release-fast -q -p pagesift-pdf --example pdf_dir
  target/release-fast/examples/pdf_dir "$out/pdfs" "$out/cand" | tee "$out/convert.txt"
else
  cargo build --profile release-fast -q -p pagesift-ocr --example pdf_ocr_dir
  target/release-fast/examples/pdf_ocr_dir "$out/pdfs" "$out/cand" "$det" "$rec" "${math[@]}" 2> "$out/times.tsv" | tee "$out/convert.txt"
fi
# 分片并发判分
k=$(nproc)
for ((i = 0; i < k; i++)); do
  (repo=$PWD; cd "$bench" && PYTHONPATH=. "$py" -I "$repo/benchmarks/mix/score_olmocr.py" "$out/data" "$out/cand" "$out/tests.$i.jsonl" "$i" "$k") &
done
wait
cat "$out"/tests.*.jsonl > "$out/tests.jsonl"
"$py" -I - "$out/tests.jsonl" <<'PY' | tee "$out/score.txt"
import json, statistics, sys
cats = {}
for line in open(sys.argv[1], encoding="utf-8"):
    r = json.loads(line)
    cats.setdefault(r["category"], []).append(r["passed"])
for c, v in sorted(cats.items()):
    print(f"{c}\t{sum(v)}/{len(v)}\t{sum(v) / len(v):.3f}")
print(f"总分（各类平均，含 baseline）\t{statistics.mean(sum(v) / len(v) for v in cats.values()):.3f}")
nob = [sum(v) / len(v) for c, v in cats.items() if c != "baseline"]
print(f"去掉 baseline\t{statistics.mean(nob):.3f}")
PY

#!/usr/bin/env bash
# B5 with OCR on the final binary: olmOCR score (with and without baseline) and pages/s per configuration.
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
stage_begin b5ocr
D=$RUN/b5ocr
mkdir -p "$D"
M=${XDG_CACHE_HOME:-$HOME/.cache}/pagesift/models
SMALL=("$M/PP-OCRv6_small_det_safetensors" "$M/PP-OCRv6_small_rec_safetensors")
MEDIUM=("$M/PP-OCRv6_medium_det_safetensors" "$M/PP-OCRv6_medium_rec_safetensors")
MATH=("$M/PP-DocLayoutV3_safetensors" "$M/PP-FormulaNet_plus-L_safetensors")
export PDF_OCR_BIN=$EX/pdf_ocr_dir

run() { # name force(0/1) detection recognition [layout formula]
    local name=$1 force=$2
    shift 2
    log "b5ocr: $name"
    if [ "$force" = 1 ]; then
        locked env PDF_OCR_FORCE=1 OCR_DEVICE=gpu benchmarks/history/1.0/g2/score_b5.sh "$D/$name" "$@" > "$D/$name.log" 2>&1
    else
        locked env OCR_DEVICE=gpu benchmarks/history/1.0/g2/score_b5.sh "$D/$name" "$@" > "$D/$name.log" 2>&1
    fi
    "$PY" -I - "$D/$name" "$name" >> "$RUN/metrics.tsv" <<'PY'
import re, sys
d, name = sys.argv[1:]
for line in open(f"{d}/score.txt", encoding="utf-8"):
    k, *rest = line.rstrip("\n").split("\t")
    v = rest[-1] if rest else ""
    key = {"总分（各类平均，含 baseline）": "olmocr", "去掉 baseline": "nobase"}.get(k, k)
    print(f"b5ocr.{name}.{key}\t{v}")
from pathlib import Path
md = [p for p in Path(f"{d}/cand").rglob("*.md") if p.read_text(encoding="utf-8", errors="replace").strip()]
print(f"b5ocr.{name}.completion\t{len(md) / 134:.6f}")
m = re.search(r"([\d.]+) pages/s", open(f"{d}/convert.txt", encoding="utf-8").read())
if m:
    print(f"b5ocr.{name}.pages_s\t{m.group(1)}")
PY
    # 只留分数和每题结果，页面输出和中间文件很大
    rm -rf "$D/$name/pdfs" "$D/$name/data" "$D/$name/cand" "$D/$name"/tests.[0-9]*.jsonl
}

run small_math_ocr 1 "${SMALL[@]}" "${MATH[@]}"
run small_math_auto 0 "${SMALL[@]}" "${MATH[@]}"
run medium_math_ocr 1 "${MEDIUM[@]}" "${MATH[@]}"
stage_end b5ocr

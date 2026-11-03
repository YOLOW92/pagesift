#!/usr/bin/env bash
# G3：B5（olmOCR-bench 的 old_scans、old_scans_math）上 CPU 和 GPU、small 和 medium 的每秒页数、显存峰值和分数。
#   flock <bench.lock> benchmarks/history/1.0/g3/run_gpu_b5.sh <输出目录> [math]
# 第二个参数给 math 时只跑带公式识别的两种 GPU 配置（CPU 对照见 README）。
# 每种配置跑一次，顺序执行。显存峰值有两个口径：nvidia-smi 每 200 ms 采样的增量（整张卡，含驱动的上下文），
# 和程序自己记的（pdf_ocr_dir 结尾打印的 peak、needed）。score_b5.sh 依赖 benchmarks/data。
set -uo pipefail
cd "$(dirname "$0")/../../../.."
out=${1:?输出目录}
mkdir -p "$out"
out=$(cd "$out" && pwd)
m=${XDG_CACHE_HOME:-$HOME/.cache}/pagesift/models
small=("$m/PP-OCRv6_small_det_safetensors" "$m/PP-OCRv6_small_rec_safetensors")
medium=("$m/PP-OCRv6_medium_det_safetensors" "$m/PP-OCRv6_medium_rec_safetensors")
gpu_mem() { nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | head -1; }

run() { # 名字 设备 并行页数 检测目录 识别目录 [版面目录 公式目录]
    local name=$1 dev=$2 jobs=$3 det=$4 rec=$5
    shift 5
    local base peak=0 v pid
    base=$(gpu_mem)
    ( while :; do gpu_mem; sleep 0.2; done > "$out/$name.vram" ) &
    pid=$!
    OCR_DEVICE=$dev OCR_JOBS=$jobs benchmarks/history/1.0/g2/score_b5.sh "$out/$name" "$det" "$rec" "$@" > "$out/$name.log" 2>&1
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    while read -r v; do [ "$v" -gt "$peak" ] && peak=$v; done < "$out/$name.vram"
    echo "== $name（设备 $dev，并行 $jobs 页）"
    grep -E '^pagesift\+ocr|^device|^gpu:' "$out/$name/convert.txt"
    echo "nvidia-smi 峰值增量 $((peak - base)) MiB（基线 $base MiB）"
    cat "$out/$name/score.txt"
    echo
}

if [ "${2:-}" = math ]; then
    math=("$m/PP-DocLayoutV3_safetensors" "$m/PP-FormulaNet_plus-L_safetensors")
    run gpu_small_math gpu 1 "${small[@]}" "${math[@]}"
    run gpu_medium_math gpu 1 "${medium[@]}" "${math[@]}"
    exit 0
fi

run cpu_small cpu 1 "${small[@]}"
run gpu_small gpu 1 "${small[@]}"
run gpu_small_j4 gpu 4 "${small[@]}"
run gpu_medium gpu 1 "${medium[@]}"
run cpu_medium cpu 1 "${medium[@]}"

echo "== CPU 和 GPU（small）的输出差异：不同的文件数 / 总文件数"
diff -rq "$out/cpu_small/cand" "$out/gpu_small/cand" | wc -l
find "$out/cpu_small/cand" -name '*.md' | wc -l
echo "== 并行 1 页和 4 页（GPU small）的输出差异：不同的文件数"
diff -rq "$out/gpu_small/cand" "$out/gpu_small_j4/cand" | wc -l

#!/usr/bin/env bash
# 模糊测试（stable 工具链，pagesift-fuzz）：每个目标按时间预算跑，每个进程限内存。
#   scripts/fuzz.sh [每个目标的秒数，默认 1200] [目标…，默认 html markdown charset pdf]
# 环境变量：FUZZ_OUT（输出目录，默认 benchmarks/fuzz-out）、FUZZ_MEM（每进程内存上限，默认 2G）、
#   FUZZ_PROCS（每个目标的进程数，默认按可用内存算，见 scripts/jobs.sh）、
#   FUZZ_SEED（种子基数，默认 7919；换一个数就是另一组输入序列）。
# 种子语料取自 benchmarks/data（不在仓库里，见 tasks/0.80/b1-b3/RESULT.md）。
# 结果：$FUZZ_OUT/summary.jsonl 每个进程一行；崩溃、超时的输入存成 crash-*.bin / timeout-*.bin；
# 进程被杀（栈溢出、超内存）时按 progress 文件重放出输入，存成 killed-*.bin。
set -u
cd "$(dirname "$0")/.."
secs=${1:-1200}
shift || true
targets=("$@")
[ ${#targets[@]} -eq 0 ] && targets=(html markdown charset pdf)
out=${FUZZ_OUT:-bench/fuzz-out}
mem=${FUZZ_MEM:-2G}
data=benchmarks/data
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/home/yolow92/pagesift/target}
cargo build --release -p pagesift-fuzz || exit 1
bin=$CARGO_TARGET_DIR/release/pagesift-fuzz
mkdir -p "$out/seeds"

# 种子清单：每类取一部分，太大的文件跳过（变异时还会再放大）
seeds() {
  local name=$1
  case $name in
    html|markdown)
      { find "$data/snapshots" -name '*.html' -size -400k | sort | awk 'NR % 8 == 0'
        find "$data/wpt-parsing" -name '*.dat' | sort
        find crates -path '*/tests/*' -name '*.html' | sort; } ;;
    charset)
      find "$data/fresh/data/chardet-test-data" -type f -size -200k ! -name '*.md' | sort | awk 'NR % 6 == 0' ;;
    pdf)
      { find "$data/pdfjs" -name '*.pdf' -size -2M | sort | awk 'NR % 4 == 0'
        find crates/pagesift-pdf -name '*.pdf' -size -2M | sort; } ;;
  esac
}

procs=${FUZZ_PROCS:-$(scripts/jobs.sh 1200 4096)}
(( procs > 4 )) && procs=4
for t in "${targets[@]}"; do
  list=$out/seeds/$t.list
  seeds "$t" | xargs -r realpath > "$list"
  echo "== $t：种子 $(wc -l < "$list") 个，$procs 个进程，每个 ${secs}s，内存上限 $mem"
  pids=()
  for ((k = 1; k <= procs; k++)); do
    seed=$(( k * ${FUZZ_SEED:-7919} ))
    (
      systemd-run --user --scope -q -p MemoryMax="$mem" -p MemorySwapMax=0 \
        "$bin" "$t" fuzz "$list" "$secs" "$out" "$seed" >> "$out/summary.jsonl"
      code=$?
      if [ $code -ne 0 ] && [ $code -ne 3 ]; then
        prog=$out/progress-$t-$seed
        iter=$(od -An -tu8 -N8 "$prog" | tr -d ' ')
        "$bin" "$t" replay "$list" "$seed" "$iter" "$out/killed-$t-$seed-$iter.bin"
        echo "{\"target\":\"$t\",\"seed\":$seed,\"result\":\"killed\",\"code\":$code,\"iter\":$iter}" >> "$out/summary.jsonl"
      fi
    ) &
    pids+=($!)
  done
  wait "${pids[@]}"
done
grep -h . "$out/summary.jsonl" | tail -n $(( ${#targets[@]} * procs ))

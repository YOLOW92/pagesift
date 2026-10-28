#!/usr/bin/env bash
# Deterministic performance counts of the HTML, PDF and CSV benchmarks against a baseline ref.
#
#   scripts/perf_counts.sh <baseline ref, e.g. v0.80.0> <fixture dir with paper.pdf, ascii.csv, sjis.csv, gbk.csv, utf8-cjk.csv>
#
# Runs cachegrind with cache simulation on a fixed cache geometry (32 KB 8-way L1 I/D, 8 MB 16-way LL,
# 64-byte lines), so the numbers do not depend on the machine or on how busy it is. Instruction counts
# alone miss cache behaviour: the same instructions walking memory out of order can be several times
# slower. The estimate is cachegrind's usual one: cycles = Ir + 10 x L1 misses + 100 x LL misses.
set -euo pipefail
BASE=$1
FIX=$(cd "$2" && pwd)
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${PERF_WORK:-$ROOT/target/perf-counts}
mkdir -p "$WORK"
EXAMPLES=(-p pagesift-content --example perf_gate -p pagesift-pdf --example pdf_one -p pagesift --example office_bench)

if [ ! -d "$WORK/base" ]; then
  git -C "$ROOT" worktree add --detach "$WORK/base" "$BASE" >/dev/null
fi
(cd "$WORK/base" && CARGO_TARGET_DIR="$WORK/base-target" cargo build -q --release "${EXAMPLES[@]}")
(cd "$ROOT" && cargo build -q --release "${EXAMPLES[@]}")

CG=(valgrind --tool=cachegrind --cache-sim=yes --I1=32768,8,64 --D1=32768,8,64 --LL=8388608,16,64)
run() {  # tag bin args...
  local tag=$1 bin=$2; shift 2
  "${CG[@]}" --cachegrind-out-file="$WORK/$tag.out" "$bin" "$@" >/dev/null 2>"$WORK/$tag.log"
}
for side in base head; do
  if [ $side = base ]; then dir=$WORK/base-target/release/examples; else dir=$ROOT/target/release/examples; fi
  run "html-$side" "$dir/perf_gate" &
  run "pdf-$side" "$dir/pdf_one" "$FIX/paper.pdf" 3 &
  run "csv-$side" "$dir/office_bench" 3 "$FIX/ascii.csv" "$FIX/sjis.csv" "$FIX/gbk.csv" "$FIX/utf8-cjk.csv" &
done
wait

python3 - "$WORK" "$BASE" <<'EOF'
import sys
work, base = sys.argv[1], sys.argv[2]
def load(tag):
    for line in open(f'{work}/{tag}.out'):
        if line.startswith('summary:'):
            ir, i1, il, dr, d1r, dlr, dw, d1w, dlw = map(int, line.split()[1:])
            l1, ll = i1 + d1r + d1w, il + dlr + dlw
            return {'instructions': ir, 'L1 misses': l1, 'LL misses': ll, 'est. cycles': ir + 10 * l1 + 100 * ll}
print(f"{'bench':<6}{'count':<14}{base:>16}{'HEAD':>16}{'change':>9}")
for b in ('html', 'pdf', 'csv'):
    a, h = load(f'{b}-base'), load(f'{b}-head')
    for k in a:
        print(f"{b:<6}{k:<14}{a[k]:>16,}{h[k]:>16,}{(h[k] / a[k] - 1) * 100:>+8.2f}%")
EOF

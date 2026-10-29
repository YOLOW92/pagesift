#!/usr/bin/env bash
# Everything that can be prepared before the 90-minute test window:
# build 0.81 and the example binaries, restore the frozen inputs, check data,
# and smoke every entry point.  This is deliberately not part of the timed run.
#
#   ./prep.sh                 # full prep; requires the 0.81 source checkout
#   ALLOW_ANY_VERSION=1 ./prep.sh   # smoke the harness with the current checkout
#
# The 0.81 binary is expected at target/release/pagesift after the release is
# published.  Until then ALLOW_ANY_VERSION=1 lets the harness be tested with
# 0.80.10; the run scripts refuse a non-0.81 binary unless that variable is set.
set -euo pipefail
cd "$(dirname "$0")"
source ./lib.sh
cd "$REPO"

check_bin
log "preparing $PAGESIFT_VERSION ($BIN)"

# ---- build ---------------------------------------------------------------
if [[ ${SKIP_BUILD:-0} != 1 ]]; then
    log "building workspace binaries"
    cargo build --release --locked -p pagesift
    cargo build --release --locked -p pagesift --examples
    cargo build --release --locked -p pagesift-content --examples
    cargo build --release --locked -p pagesift-pdf --examples
    cargo build --release --locked -p pagesift-extract --examples
    log "building crawl origin and scale runner"
    cargo build --release --locked --manifest-path "$REPO/benchmarks/crawl/Cargo.toml"
    cargo build --release --offline --manifest-path "$REPO/benchmarks/scale/p0/Cargo.toml"  # the lock tracks workspace versions
fi
check_examples

# ---- frozen inputs -------------------------------------------------------
if [[ ${SKIP_RESTORE:-0} != 1 ]]; then
    log "restoring frozen inputs (B2 is network-bound; cached files are reused)"
    python3 -I "$HIST_DIR/restore_inputs.py" a1 a4 b3 b5 b2
fi

check_count() {
    local sub=$1 want=$2
    local got
    got=$(wc -l < "$MIX/inputs/$sub.manifest")
    [[ "$got" == "$want" ]] || die "$sub: $got inputs, expected $want"
    log "$sub: $got inputs OK"
}
check_count a1 600
check_count a2 269
check_count a4 151
check_count b1 1269
check_count b2 71
check_count b3 100
check_count b4 81
check_count b5 134

# ---- data and tool environments -----------------------------------------
for p in \
    benchmarks/data/inputs/manifest.jsonl \
    benchmarks/data/fresh/data/htmldate/tests/eval_default.json \
    benchmarks/data/fresh/data/chunkeval/questions_df.csv \
    benchmarks/data/olmocr_1403/bench_data/arxiv_math.jsonl \
    benchmarks/data/crawl-pages/1.html \
    benchmarks/data/fresh/tools/olmocr-bench/olmocr/bench/benchmark.py; do
    [[ -e "$p" ]] || die "missing benchmark data: $p"
done
[[ -x "$PY" ]] || die "missing python env: $PY"
[[ -x "$C4A" ]] || die "missing crawler python env: $C4A"
firefox=$(find "$PLAYWRIGHT_BROWSERS_PATH" -maxdepth 1 -type d -name 'firefox-*' | head -1)
[[ -n "$firefox" ]] || die "missing Playwright Firefox in $PLAYWRIGHT_BROWSERS_PATH (run: $C4A -m playwright install firefox)"
log "Playwright Firefox: $firefox"

# ---- smoke ---------------------------------------------------------------
S=$RUN/smoke
mkdir -p "$S"
head -3 "$MIX/inputs/a2.manifest" > "$S/a2-3.txt"
head -3 "$MIX/inputs/b1.manifest" > "$S/b1-3.txt"
head -3 "$MIX/inputs/b4.manifest" > "$S/b4-3.txt"

log "smoke: pagesift CLI, serve, PDF, Office"
MIX_PAGESIFT_BIN=$BIN python3 -I benchmarks/mix/driver.py --tool pagesift-081 --manifest "$S/a2-3.txt" --out "$S/a2" --workers 1 > "$S/a2.json" 2> "$S/a2.log"
MIX_PAGESIFT_BIN=$BIN python3 -I benchmarks/mix/driver.py --tool pagesift-081-serve --manifest "$S/a2-3.txt" --out "$S/a2-serve" --workers 1 > "$S/a2-serve.json" 2> "$S/a2-serve.log"
MIX_PAGESIFT_BIN=$BIN python3 -I benchmarks/mix/driver.py --tool pagesift-081-file --manifest "$S/b1-3.txt" --out "$S/b1" --workers 1 > "$S/b1.json" 2> "$S/b1.log"
MIX_PAGESIFT_BIN=$BIN python3 -I benchmarks/mix/driver.py --tool pagesift-081-file --manifest "$S/b4-3.txt" --out "$S/b4" --workers 1 > "$S/b4.json" 2> "$S/b4.log"
for pair in a2:pagesift-081 a2-serve:pagesift-081-serve b1:pagesift-081-file b4:pagesift-081-file; do
    d=${pair%%:*}; tool=${pair#*:}
    f="$S/$d/$tool/results.jsonl"
    [[ -f "$f" ]] || die "smoke $d produced no results at $f"
    [[ $(wc -l < "$f") == 3 ]] || die "smoke $d did not produce 3 results"
    ! grep -q '"status": "error"' "$f" || die "smoke $d has an error"
done

log "smoke: domain entry points"
python3 -I - "$REPO/benchmarks/data/inputs/manifest.jsonl" "$S/extract.jsonl" "$REPO" <<'PY'
import json, sys
src, dst, repo = sys.argv[1], sys.argv[2], sys.argv[3]
out = []
for line in open(src, encoding="utf-8"):
    r = json.loads(line)
    r["path"] = f"{repo}/benchmarks/data/{r['path'].removeprefix('bench/data/')}"
    out.append(r)
with open(dst, "w", encoding="utf-8") as f:
    for r in out[:5]:
        f.write(json.dumps(r, ensure_ascii=False) + "\n")
PY
"$EX/extract_jsonl" "$S/extract.jsonl" "$S/extract.out.jsonl" > "$S/extract.txt" 2>&1
[[ $(wc -l < "$S/extract.out.jsonl") == 5 ]] || die "extract_jsonl smoke failed"

"$PY" -I - "$REPO" "$S/dates.list" <<'PY'
import json, sys
repo, out = sys.argv[1], sys.argv[2]
d = json.load(open(f"{repo}/benchmarks/data/fresh/data/htmldate/tests/eval_default.json", encoding="utf-8"))
paths = []
for url, v in d.items():
    for sub in ("test_set", "cache", "eval"):
        p = f"{repo}/benchmarks/data/fresh/data/htmldate/tests/{sub}/{v['file']}"
        if __import__("os").path.isfile(p):
            paths.append(p)
            break
open(out, "w").write("\n".join(paths[:3]) + "\n")
PY
"$EX/meta_eval" "$S/dates.list" > "$S/dates.jsonl"
[[ $(wc -l < "$S/dates.jsonl") == 3 ]] || die "meta_eval smoke failed"

"$EX/pdf_dir" "$REPO/benchmarks/data/olmocr_1403/bench_data/pdfs/arxiv_math" "$S/pdf" > "$S/pdf.txt" 2>&1
[[ $(find "$S/pdf" -name '*.md' | wc -l) -gt 0 ]] || die "pdf_dir smoke failed"
"$EX/office_bench" 1 "$(head -1 "$S/b4-3.txt" | cut -f2)" > "$S/office.txt" 2>&1
[[ -s "$S/office.txt" ]] || die "office_bench smoke failed"

log "smoke: crawl origin"
( cd benchmarks/crawl && PAGESIFT_BIN=$EX/crawl_bench MEASURE_OUT="$S/crawl.jsonl" TOOLS=pagesift ./run_all.sh smoke ) > "$S/crawl.log" 2>&1
grep -q '"pages_per_s"' "$S/crawl.jsonl" || die "crawl smoke failed"

log "smoke: browser auto (3 sites x 10 pages)"
( cd benchmarks/crawl && PAGESIFT_BIN=$EX/crawl_bench MEASURE_OUT="$S/browser.jsonl" ENGINES=auto ./engines.sh smoke ) > "$S/browser.log" 2>&1
grep -q '"pages_per_s"' "$S/browser.jsonl" || die "browser smoke failed"

{
    echo "prepared_at	$(date -Is)"
    echo "git_head	$(git -C "$REPO" rev-parse HEAD)"
    echo "pagesift_version	$PAGESIFT_VERSION"
    echo "pagesift_sha256	$(sha256sum "$BIN" | cut -d' ' -f1)"
    echo "run	$RUN"
    echo "archive	$ARCHIVE"
} > "$RUN/prep.txt"
cp "$RUN/prep.txt" "$ARCHIVE/prep.txt"
mkdir -p "$MIX/inputs/manifests"
cp -f "$RUN"/manifests/*.tsv "$MIX/inputs/manifests/" 2>/dev/null || true
cp -f "$RUN/prep.txt" "$MIX/inputs/prep.txt"
log "prep OK; smoke evidence in $S"

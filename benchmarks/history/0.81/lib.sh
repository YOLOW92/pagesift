#!/usr/bin/env bash
# 0.81 per-domain refresh: shared paths, checks and helpers.
# Sourced by every script in this directory. Keep it free of side effects beyond
# creating the run and archive directories.
set -euo pipefail

HIST_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HIST_DIR/../../.." && pwd)
MIX=${MIX_TOOLS:-$HOME/mix-tools}
BIN=${PAGESIFT_BIN:-$REPO/target/release/pagesift}
EX=${PAGESIFT_EXAMPLES:-$REPO/target/release/examples}
CRAWL_BIN=${CRAWL_BIN:-$REPO/benchmarks/crawl/target/release}
SCALE_BIN=${SCALE_BIN:-$REPO/benchmarks/scale/p0/target/release}
LOCK=${BENCH_LOCK:-$REPO/tasks/0.80/bench.lock}

# Keep the parent's run/archive when a domain script is launched by run.sh;
# only create a new stamp at the top level (or when RUN_ID is given).
STAMP=${RUN_ID:-${STAMP:-$(date +%Y%m%d-%H%M%S)}}
RUN=${RUN_ROOT:-${RUN:-$MIX/runs-081/$STAMP}}
ARCHIVE=${ARCHIVE_ROOT:-${ARCHIVE:-$HIST_DIR/results/$STAMP}}

export HIST_DIR REPO MIX BIN EX CRAWL_BIN SCALE_BIN LOCK STAMP RUN ARCHIVE
mkdir -p "$RUN" "$ARCHIVE"

PY=${PY:-$REPO/benchmarks/data/fresh/tools/py/bin/python}
C4A=${C4A:-$REPO/benchmarks/data/fresh/tools/c4ai/bin/python}
PLAYWRIGHT_BROWSERS_PATH=${PLAYWRIGHT_BROWSERS_PATH:-$MIX/ms-playwright}
export PY C4A PLAYWRIGHT_BROWSERS_PATH

log() { printf '%s %s\n' "$(date +%T)" "$*" >&2; }
die() { log "ERROR: $*"; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing command: $1"; }

check_bin() {
    [ -x "$BIN" ] || die "missing 0.81 binary: $BIN (run prep.sh after the release is published)"
    local v
    v=$("$BIN" --version 2>/dev/null | head -1 || true)
    if [[ "$v" != *"0.81"* && ${ALLOW_ANY_VERSION:-0} != 1 ]]; then
        die "binary is '$v', expected 0.81; set ALLOW_ANY_VERSION=1 only for a smoke run"
    fi
    export PAGESIFT_VERSION="$v"
}

check_examples() {
    local e
    for e in meta_eval md_convert chunk_spans qf_ranks pdf_dir office_bench crawl_bench; do
        [ -x "$EX/$e" ] || die "missing example $EX/$e (run prep.sh)"
    done
    [ -x "$CRAWL_BIN/origin" ] || die "missing $CRAWL_BIN/origin (run prep.sh)"
}

# One benchmark at a time on the shared machine. run_batch.py manages the lock
# itself for driver jobs; use locked() only around direct measurements.
locked() { flock "$LOCK" "$@"; }

emit() { printf '%s\t%s\n' "$1" "$2" >> "$RUN/metrics.tsv"; }

stage_begin() {
    local name=$1
    date +%s > "$RUN/.stage-$name.start"
    log "== stage $name"
}

stage_end() {
    local name=$1
    local start end
    start=$(cat "$RUN/.stage-$name.start" 2>/dev/null || echo 0)
    end=$(date +%s)
    printf '%s\t%s\n' "$name" "$((end - start))" >> "$RUN/stage-times.tsv"
    log "== stage $name done in $((end - start))s"
}

# Parse a key from a JSON file with Python (no jq dependency for nested data).
jget() { "$PY" -I -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2], {"d": d}))' "$1" "$2"; }

ensure_dir() { mkdir -p "$1"; }

# Write a JSON manifest of paths for the per-domain examples from a mix manifest.
# mix manifests use id<TAB>path<TAB>url; the paths are absolute already.
mix_manifest() {
    local sub=$1 out=$2
    [ -f "$MIX/inputs/$sub.manifest" ] || die "missing $MIX/inputs/$sub.manifest"
    cp "$MIX/inputs/$sub.manifest" "$out"
    printf '%s\n' "$(wc -l < "$out")"
}

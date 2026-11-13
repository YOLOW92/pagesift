#!/usr/bin/env bash
# 1.4 in-process benchmarks through PageSift's standalone crates:
#   - pagesift-syntax (yolowsyntax 0.1.2) on 8 real OSS files
#   - pagesift-compress (yolowzstd 0.1.2) encode/decode on Silesia payloads
#   - pagesift-office (yolowdoc 0.1.1) covered by office.sh and v2_quality/v2_speed
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin yolow_deps

D=$RUN/yolow_deps
mkdir -p "$D/zstd"

# 1. pagesift-syntax (yolowsyntax 0.1.2)
REAL_SYNTAX=/home/yolow92/yolow/yolowsyntax/benchmarks/corpus/_real
SYNTAX_INPUTS=()
for f in c.c go.go java.java python.py javascript.js typescript.ts rust.rs bash.sh; do
    if [[ -f "$REAL_SYNTAX/$f" ]]; then
        SYNTAX_INPUTS+=("$REAL_SYNTAX/$f")
    fi
done
if [[ ${#SYNTAX_INPUTS[@]} -eq 0 ]]; then
    SYNTAX_INPUTS=(
        crates/pagesift-graph/src/builder.rs
        crates/pagesift-graph/src/citation.rs
        benchmarks/mix/score_quality.py
        crates/pagesift-node/index.d.ts
        benchmarks/history/1.4/run.sh
    )
fi

log "yolow_deps: pagesift-syntax (yolowsyntax 0.1.2) on ${#SYNTAX_INPUTS[@]} files"
locked taskset -c 4 "$EX/syntax_bench" 25 "${SYNTAX_INPUTS[@]}" > "$D/syntax.tsv"

# 2. pagesift-compress (yolowzstd 0.1.2)
SILESIA=/home/yolow92/yolow/yolowzstd/benchmarks/corpus/silesia
ZSTD_FILES=()
for f in dickens mozilla ooffice xml; do
    if [[ -f "$SILESIA/$f" ]]; then
        ZSTD_FILES+=("$SILESIA/$f")
    fi
done
if [[ ${#ZSTD_FILES[@]} -eq 0 ]]; then
    cat crates/pagesift-graph/src/*.rs > "$D/zstd/payload.txt"
    ZSTD_FILES=("$D/zstd/payload.txt")
fi

ZSTD_ARGS=("${ZSTD_FILES[@]}")
if command -v zstd >/dev/null 2>&1; then
    for f in "${ZSTD_FILES[@]}"; do
        out="$D/zstd/$(basename "$f").zst"
        zstd -3 -q -f "$f" -o "$out"
        ZSTD_ARGS+=("$out")
    done
fi

log "yolow_deps: pagesift-compress (yolowzstd 0.1.2) L3/L9 + decode"
PAGESIFT_COMPRESS_LEVELS=3,9 locked taskset -c 4 "$EX/measure" "${ZSTD_ARGS[@]}" > "$D/zstd.txt"

"$PY" -I - "$D/syntax.tsv" "$D/zstd.txt" <<'PY' >> "$RUN/metrics.tsv"
import re, statistics, sys
from pathlib import Path

syn_lines = [l.split("\t") for l in Path(sys.argv[1]).read_text().splitlines() if l.strip()]
syn_mbs = [float(c[4]) for c in syn_lines]
syn_lossless = all(c[7] == "true" for c in syn_lines)
syn_rss = max((int(c[8]) / 1024.0 for c in syn_lines), default=0.0)
for c in syn_lines:
    lang = c[1]
    print(f"deps.syntax.{lang}.ms\t{c[3]}")
    print(f"deps.syntax.{lang}.mb_s\t{c[4]}")
if syn_mbs:
    print(f"deps.syntax.median_mb_s\t{statistics.median(syn_mbs):.3f}")
    print(f"deps.syntax.min_mb_s\t{min(syn_mbs):.3f}")
    print(f"deps.syntax.max_mb_s\t{max(syn_mbs):.3f}")
print(f"deps.syntax.lossless_ok\t{1 if syn_lossless else 0}")
print(f"deps.syntax.peak_rss_mb\t{syn_rss:.2f}")

enc3, enc9, dec, ratios3 = [], [], [], []
raw_sizes = {}
for line in Path(sys.argv[2]).read_text().splitlines():
    m_enc = re.match(r"(.+?)\s+level\s+(\d+):\s+(\d+)\s+bytes,\s+([0-9.]+)\s+MB/s", line)
    if m_enc:
        path, lvl, zbytes, mbs = m_enc.group(1), int(m_enc.group(2)), int(m_enc.group(3)), float(m_enc.group(4))
        name = Path(path).name
        raw_len = raw_sizes.setdefault(name, Path(path).stat().st_size if Path(path).exists() else 0)
        if lvl == 3:
            enc3.append(mbs)
            if raw_len and zbytes:
                ratios3.append(raw_len / zbytes)
            print(f"deps.zstd.{name}.l3_mb_s\t{mbs}")
        elif lvl == 9:
            enc9.append(mbs)
            print(f"deps.zstd.{name}.l9_mb_s\t{mbs}")
        continue
    m_dec = re.match(r"(.+?)\s+decode\s+([0-9.]+)\s+MB/s", line)
    if m_dec:
        path, mbs = m_dec.group(1), float(m_dec.group(2))
        name = Path(path).name.removesuffix(".zst")
        dec.append(mbs)
        print(f"deps.zstd.{name}.decode_mb_s\t{mbs}")

if enc3:
    print(f"deps.zstd.l3_median_mb_s\t{statistics.median(enc3):.2f}")
if enc9:
    print(f"deps.zstd.l9_median_mb_s\t{statistics.median(enc9):.2f}")
if dec:
    print(f"deps.zstd.decode_median_mb_s\t{statistics.median(dec):.2f}")
if ratios3:
    print(f"deps.zstd.l3_median_ratio\t{statistics.median(ratios3):.2f}")
PY

rm -rf "$D/zstd"
stage_end yolow_deps

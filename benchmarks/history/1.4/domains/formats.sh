#!/usr/bin/env bash
# Format smoke suite backed by yolowdoc 0.1.1 + yolowsyntax 0.1.2 (373 languages + SQLite UTF-8/UTF-16).
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin formats

D=$RUN/formats
mkdir -p "$D"
log "formats: running smoke.py"
"$PY" -I benchmarks/formats/smoke.py --pagesift "$BIN" | tee "$D/smoke.txt"

"$PY" -I - "$D/smoke.txt" <<'PY' >> "$RUN/metrics.tsv"
import re, sys
from pathlib import Path

txt = Path(sys.argv[1]).read_text(encoding="utf-8")
m = re.search(r"代码语言：(\d+)\s*种，(\d+)\s*个扩展名，(\d+)\s*个文件名，失败\s*(\d+)", txt)
if m:
    print(f"formats.code_languages\t{m.group(1)}")
    print(f"formats.code_extensions\t{m.group(2)}")
    print(f"formats.code_filenames\t{m.group(3)}")
    print(f"formats.code_failed\t{m.group(4)}")
ok = 1
for enc in ("UTF-8", "UTF-16le", "UTF-16be"):
    em = re.search(rf"SQLite {enc}：退出码\s*(\d+)，people 缺\s*(\d+)/\d+，kv 缺\s*(\d+)/\d+", txt)
    if not em or any(int(g) != 0 for g in em.groups()):
        ok = 0
print(f"formats.sqlite_ok\t{ok}")
PY

stage_end formats

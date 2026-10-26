#!/bin/bash
# D1 正文提取对决：所有参赛者统一 16 分片、10 GiB 内存上限、16 逻辑核。
# 每项单独计时并轮询 docker stats 记录峰值内存；OOM（exit 137）与超时都记为失败。
set -u
cd ~/pagesift
OUT=$PWD/out/d1; mkdir -p "$OUT"
MAN=/data/showdown/d1/manifest.jsonl
NPROC=16; MEM=10g
UIDGID="$(id -u):$(id -g)"
RUST70=$PWD/benchmarks/web-extraction/compare/rust-tools/target/release/pagesift-compare-rust
RUST50=$PWD/.worktree/050/benchmarks/web-extraction/compare/rust-tools/target/release/pagesift-compare-rust
GOBIN=$PWD/benchmarks/thirdparty/bin
MNT="-v $PWD/benchmarks/data:/data:ro -v $PWD/benchmarks/web-extraction/compare:/cmp:ro -v $PWD/benchmarks/web-extraction/scripts:/tools:ro -v $PWD/out:/out"

peak_poll () { # cname outfile
  : > "$2"
  # 容器是异步创建的：先等它出现，再开始采样，否则会立刻退出
  for _ in $(seq 1 200); do docker inspect "$1" >/dev/null 2>&1 && break; sleep 0.05; done
  while docker inspect "$1" >/dev/null 2>&1; do
    docker stats --no-stream --format '{{.MemUsage}}' "$1" 2>/dev/null | awk '{print $1}' >> "$2"
    sleep 0.15
  done
}

run () { # tag image extra_mounts command
  local tag=$1 image=$2 mounts=$3 cmd=$4
  local cname="d1-$(echo "$tag" | tr -c 'a-zA-Z0-9' '-')"
  docker rm -f "$cname" >/dev/null 2>&1
  local pf="$OUT/.$tag.peak"
  peak_poll "$cname" "$pf" & local pp=$!
  local t0=$(date +%s%N)
  docker run --name "$cname" --user "$UIDGID" --network=host \
      --memory=$MEM --memory-swap=$MEM --cpuset-cpus=0-15 \
      $MNT $mounts $image sh -lc "$cmd; echo PEAK_BYTES=\$(cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo 0)" > "$OUT/$tag.log" 2>&1
  local rc=$?
  local t1=$(date +%s%N)
  kill $pp 2>/dev/null; wait $pp 2>/dev/null
  local wall=$(awk -v a=$t0 -v b=$t1 'BEGIN{printf "%.3f",(b-a)/1e9}')
  local rows=$(wc -l < "$OUT/$tag.jsonl" 2>/dev/null || echo 0)
  local peak=$(python3 - "$pf" <<'PY'
import re,sys
best=0
for l in open(sys.argv[1], errors="ignore"):
    m=re.match(r'([\d.]+)\s*([kMG]i?B)', l.strip())
    if not m: continue
    v=float(m.group(1)); u=m.group(2)
    mult={'B':1,'KiB':1024,'MiB':1024**2,'GiB':1024**3,'kB':1000,'MB':1000**2,'GB':1000**3}.get(u,1)
    best=max(best,int(v*mult))
print(int(sys.argv[2]) if len(sys.argv)>2 and sys.argv[2].isdigit() and int(sys.argv[2])>best else best)
PY
"${cgpeak:-0}")
  local oom=false; [ $rc -eq 137 ] && oom=true
  echo "{\"contestant\":\"$tag\",\"exit\":$rc,\"oom\":$oom,\"wall_s\":$wall,\"rows\":$rows,\"peak_rss_bytes\":${peak:-0}}" > "$OUT/$tag.meta.json"
  printf "%-20s exit=%-3s %8.2fs rows=%-5s peak=%.0fMiB%s\n" "$tag" "$rc" "$wall" "$rows" \
     "$(echo "${peak:-0}/1048576" | bc -l 2>/dev/null || echo 0)" "$([ "$oom" = true ] && echo ' OOM')"
  docker rm -f "$cname" >/dev/null 2>&1
}

SHARD_NATIVE='set -e; T=$(mktemp -d); split -n l/16 -d -a 3 $MAN $T/s.; for f in $T/s.*; do "$BIN" $TOOLARG $f $f.out & done; wait; cat $T/s.*.out > OUTPATH'

# ---- PageSift 0.70 / 0.50 ----
run pagesift-070 pagesift-bench:native "-v $RUST70:/rs:ro" \
  'set -e; T=$(mktemp -d); split -n l/16 -d -a 3 /data/showdown/d1/manifest.jsonl $T/s.; for f in $T/s.*; do /rs pagesift $f $f.out & done; wait; cat $T/s.*.out > /out/d1/pagesift-070.jsonl'
run pagesift-050 pagesift-bench:native "-v $RUST50:/rs:ro" \
  'set -e; T=$(mktemp -d); split -n l/16 -d -a 3 /data/showdown/d1/manifest.jsonl $T/s.; for f in $T/s.*; do /rs pagesift $f $f.out & done; wait; cat $T/s.*.out > /out/d1/pagesift-050.jsonl'

# ---- Rust 竞品 ----
for t in crw dom_smoothie htmd html-to-markdown-rs rs-trafilatura; do
  run "$t" pagesift-bench:native "-v $RUST70:/rs:ro" \
    "set -e; T=\$(mktemp -d); split -n l/16 -d -a 3 /data/showdown/d1/manifest.jsonl \$T/s.; for f in \$T/s.*; do /rs $t \$f \$f.out & done; wait; cat \$T/s.*.out > /out/d1/$t.jsonl"
done

# ---- Go 竞品 ----
for t in readability html2md; do
  run "go-$t" pagesift-bench:native "-v $GOBIN/goadapter:/go-ad:ro" \
    "set -e; T=\$(mktemp -d); split -n l/16 -d -a 3 /data/showdown/d1/manifest.jsonl \$T/s.; for f in \$T/s.*; do /go-ad --tool $t --manifest \$f --out \$f.out --workers 1 & done; wait; cat \$T/s.*.out > /out/d1/go-$t.jsonl"
done

# ---- Python 竞品 ----
for t in trafilatura readability-lxml goose3 boilerpy3 justext resiliparse html2text markdownify; do
  run "py-$t" pagesift-bench:pytools "" \
    "set -e; T=\$(mktemp -d); split -n l/16 -d -a 3 /data/showdown/d1/manifest.jsonl \$T/s.; for f in \$T/s.*; do python /cmp/py_adapter.py $t \$f \$f.out & done; wait; cat \$T/s.*.out > /out/d1/py-$t.jsonl"
done
echo D1_RUN_DONE

#!/bin/bash
# D9 动态渲染：PageSift http / browser 两种引擎 + Playwright 对照
cd ~/pagesift
DC="docker run --rm --network=host --memory=10g --memory-swap=10g --shm-size=2g \
    --security-opt seccomp=unconfined --cap-add=SYS_ADMIN --cpuset-cpus=0-15"
PAGES="static csr delay scroll click canvas hydration"
OUT=out/d9
mkdir -p $OUT

echo "# page,engine,marker,seconds" > $OUT/pagesift.csv
for eng in http browser; do
  for p in $PAGES; do
    T0=$(date +%s%N)
    M=$($DC --user $(id -u):$(id -g) -e HOME=/tmp \
        -v $PWD/target/release/pagesift:/ps:ro -v $PWD/bench/scripts:/tools:ro \
        pagesift-bench:browser bash -c "/ps --config /tools/d9.toml scrape http://127.0.0.1:8200/$p --engine $eng 2>&1" \
        | grep -oE "MARKER-[A-Z]+" | head -1)
    T1=$(date +%s%N)
    S=$(awk -v a=$T0 -v b=$T1 'BEGIN{printf "%.2f",(b-a)/1e9}')
    echo "$p,$eng,${M:-none},$S" >> $OUT/pagesift.csv
    printf "  %-10s %-8s %-16s %6ss\n" "$p" "$eng" "${M:-none}" "$S"
  done
done
echo D9_PAGESIFT_DONE

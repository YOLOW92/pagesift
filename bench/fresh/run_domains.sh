#!/usr/bin/env bash
# 发布前的分领域全量对比：一个领域跑完再跑下一个（计时互不干扰），随机种子都由主种子派生。
# 用法：bench/fresh/run_domains.sh <结果目录> [领域...]（默认 markdown rag dates pdf）
set -uo pipefail
cd "$(dirname "$0")/../.."
R=$1; shift
mkdir -p "$R"
SEED=$(python3 -c "import json,sys; print(json.load(open('$R/seed.json'))['master_seed'])")
read -r MD1 MD2 MD3 WILD <<<"$(python3 -c "
import random; r=random.Random($SEED)
print(*[r.randrange(1, 10**6) for _ in range(4)])")"
export C4A=bench/data/fresh/tools/c4ai/bin/python
for d in ${@:-markdown rag dates pdf}; do
    echo "== $d $(date +%T)"
    case $d in
        markdown) OUT=$R/markdown bench/markdown/run.sh $MD1 $MD2 $MD3 ;;
        rag) OUT=$R/rag bench/rag/run.sh ;;
        dates) cargo build --release -q -p pagesift --example meta_eval &&
               bench/data/fresh/tools/py/bin/python -I bench/meta/dates.py > $R/dates.txt 2>&1; tail -30 $R/dates.txt ;;
        pdf) bench/pdf/run.sh && mkdir -p $R/pdf && cp bench/pdf/out/score-*.txt bench/pdf/out/*timing*.txt $R/pdf/ ;;
        wild) (cd bench/crawl && SEED=$WILD OUT=../../$R/wild PAGES_DIR=../../bench/data/random/pages \
               C4A=../../$C4A CRW=../../bench/data/fresh/tools/crw/crw ./run_wild.sh) ;;
    esac
    echo "== $d done $(date +%T)"
done

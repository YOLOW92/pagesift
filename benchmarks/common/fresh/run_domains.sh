#!/usr/bin/env bash
# 发布前的分领域全量对比：一个领域跑完再跑下一个（计时互不干扰），随机种子都由主种子派生。
# 用法：benchmarks/common/fresh/run_domains.sh <结果目录> [领域...]（默认 markdown rag qasper dates pdf wild）
# 单线程计时都经 benchmarks/common/fresh/pinned.sh 绑到不同的物理核上；这些领域同时开跑，共用同一组物理核。
# wild（爬取模拟）要独占机器：放在最后单独调用。
set -uo pipefail
cd "$(dirname "$0")/../../.."
R=$1; shift
mkdir -p "$R"
SEED=$(python3 -c "import json,sys; print(json.load(open('$R/seed.json'))['master_seed'])")
read -r MD1 MD2 MD3 WILD <<<"$(python3 -c "
import random; r=random.Random($SEED)
print(*[r.randrange(1, 10**6) for _ in range(4)])")"
export C4A=benchmarks/data/fresh/tools/c4ai/bin/python
benchmarks/common/fresh/pinned.sh --cores
for d in ${@:-markdown rag qasper dates pdf wild}; do
  [ "$d" = wild ] && wait   # 爬取模拟要独占机器：等其他领域跑完
  (
    echo "== $d $(date +%T)"
    case $d in
        markdown) OUT=$R/markdown benchmarks/markdown/run.sh $MD1 $MD2 $MD3 ;;
        rag) OUT=$R/rag benchmarks/rag/run.sh ;;
        qasper) OUT=$R/rag-qasper DATA=benchmarks/data/fresh/data/qasper-eval benchmarks/rag/run.sh ;;
        dates) cargo build --release -q -p pagesift --example meta_eval && mkdir -p $R/dates &&
               for t in pagesift htmldate trafilatura newspaper4k goose3; do
                   echo "benchmarks/data/fresh/tools/py/bin/python -I benchmarks/dates/dates.py --tools $t > $R/dates/$t.txt 2>&1"
               done | benchmarks/common/fresh/pinned.sh; cat $R/dates/*.txt | grep -v "^tool" | sort -u > $R/dates.txt; cat $R/dates.txt ;;
        pdf) benchmarks/pdf/run.sh && mkdir -p $R/pdf && cp benchmarks/pdf/out/score-*.txt benchmarks/pdf/out/*timing*.txt $R/pdf/ ;;
        wild) A=$(realpath .); RA=$(realpath "$R")
              (cd benchmarks/crawl && SEED=$WILD OUT=$RA/wild PAGES_DIR=$A/benchmarks/data/random/utf8 \
               C4A=$A/$C4A CRW=$A/benchmarks/data/fresh/tools/crw/crw ./run_wild.sh) ;;
    esac
    echo "== $d done $(date +%T)"
  ) &
  # 只用单线程计时的领域同时跑（共用 pinned.sh 的物理核池）
  [ "$d" = wild ] && wait
done
wait

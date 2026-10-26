#!/usr/bin/env bash
# 发布前的分领域全量对比：一个领域跑完再跑下一个（计时互不干扰），随机种子都由主种子派生。
# 用法：bench/fresh/run_domains.sh <结果目录> [领域...]（默认 markdown rag qasper dates pdf wild）
# 单线程计时都经 bench/fresh/pinned.sh 绑到不同的物理核上；这些领域同时开跑，共用同一组物理核。
# wild（爬取模拟）要独占机器：放在最后单独调用。
set -uo pipefail
cd "$(dirname "$0")/../.."
R=$1; shift
mkdir -p "$R"
SEED=$(python3 -c "import json,sys; print(json.load(open('$R/seed.json'))['master_seed'])")
read -r MD1 MD2 MD3 WILD <<<"$(python3 -c "
import random; r=random.Random($SEED)
print(*[r.randrange(1, 10**6) for _ in range(4)])")"
export C4A=bench/data/fresh/tools/c4ai/bin/python
bench/fresh/pinned.sh --cores
for d in ${@:-markdown rag qasper dates pdf wild}; do
  [ "$d" = wild ] && wait   # 爬取模拟要独占机器：等其他领域跑完
  (
    echo "== $d $(date +%T)"
    case $d in
        markdown) OUT=$R/markdown bench/markdown/run.sh $MD1 $MD2 $MD3 ;;
        rag) OUT=$R/rag bench/rag/run.sh ;;
        qasper) OUT=$R/rag-qasper DATA=bench/data/fresh/data/qasper-eval bench/rag/run.sh ;;
        dates) cargo build --release -q -p pagesift --example meta_eval && mkdir -p $R/dates &&
               for t in pagesift htmldate trafilatura newspaper4k goose3; do
                   echo "bench/data/fresh/tools/py/bin/python -I bench/meta/dates.py --tools $t > $R/dates/$t.txt 2>&1"
               done | bench/fresh/pinned.sh; cat $R/dates/*.txt | grep -v "^tool" | sort -u > $R/dates.txt; cat $R/dates.txt ;;
        pdf) bench/pdf/run.sh && mkdir -p $R/pdf && cp bench/pdf/out/score-*.txt bench/pdf/out/*timing*.txt $R/pdf/ ;;
        wild) A=$(realpath .); RA=$(realpath "$R")
              (cd bench/crawl && SEED=$WILD OUT=$RA/wild PAGES_DIR=$A/bench/data/random/utf8 \
               C4A=$A/$C4A CRW=$A/bench/data/fresh/tools/crw/crw ./run_wild.sh) ;;
    esac
    echo "== $d done $(date +%T)"
  ) &
  # 只用单线程计时的领域同时跑（共用 pinned.sh 的物理核池）
  [ "$d" = wild ] && wait
done
wait

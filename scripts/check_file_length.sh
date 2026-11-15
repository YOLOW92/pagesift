#!/usr/bin/env bash
# 检查是否有多个功能/职责堆砌在单个超大 .rs 文件中。
# 初衷是防止“多职责复合文件”，而非机械卡行数：
# 默认阈值 1000 行；一旦触发告警（输出 [WARN]）直接禁止通过（exit 1），
# 必须核查究竟是“单一功能的单文件”还是“多个功能的复合文件”：
# - 多职责复合文件：必须按职责拆分整改；
# - 经复核确认为单一职责的特例单文件：可在文件前 20 行标注 `// file-length-ok: <单一职责说明>` 放行。
set -euo pipefail
cd "$(dirname "$0")/.."

LIMIT=1000
for arg in "$@"; do
    case "$arg" in
        [0-9]*) LIMIT="$arg" ;;
        *) echo "用法: $0 [阈值行数，默认 1000]" >&2; exit 2 ;;
    esac
done

HIT=0
EXCLUDE_REGEX='entities\.rs$|crates/pagesift-rustls/|/tests\.rs$|_tests\.rs$|/tests/'

echo "==> 检查 crates/*/src/**/*.rs 单文件职责聚合度（非测试文件阈值: <= ${LIMIT} 行）..."

while IFS= read -r line; do
    count=$(echo "$line" | awk '{print $1}')
    file=$(echo "$line" | awk '{print $2}')

    if echo "$file" | grep -qE "$EXCLUDE_REGEX"; then
        continue
    fi

    if [ "$count" -gt "$LIMIT" ]; then
        if head -n 20 "$file" | grep -q 'file-length-ok:'; then
            reason=$(head -n 20 "$file" | grep -m1 'file-length-ok:' | sed 's/.*file-length-ok:[[:space:]]*//')
            echo "  [ALLOW] $file: $count 行（已复核单一职责特例: ${reason}）"
            continue
        fi
        echo "  [WARN] $file: $count 行 (超出 ${LIMIT} 行 $(($count - $LIMIT)) 行)"
        echo "         -> 请复核：是单一功能单文件还是多职责复合文件？复合文件须按职责整改拆分；确认单一职责可加 // file-length-ok: <原因> 放行。"
        HIT=$((HIT + 1))
    fi
done < <(find crates/*/src -name "*.rs" -type f -exec wc -l {} + | sort -rn | grep -v ' total$')

echo ""
if [ "$HIT" -eq 0 ]; then
    echo "✅ 全部非测试源文件均符合单文件职责检查（<= ${LIMIT} 行或已复核单一职责特例）"
    exit 0
else
    echo "❌ 发现 $HIT 个文件触发 [WARN]（> ${LIMIT} 行），已禁止直接通过；请核查是否为多职责复合文件。"
    exit 1
fi

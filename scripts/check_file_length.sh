#!/usr/bin/env bash
# 检查所有 workspace crates 的源文件行数。
# 单个 .rs 源文件严控在 500 行以内（数据查找表除外）。
set -euo pipefail
cd "$(dirname "$0")/.."

MAX_LINES=${1:-500}
FAILED=0
EXCLUDE_REGEX="entities\.rs"

echo "==> 检查 crates/*/src/**/*.rs 文件行数（限制: <= ${MAX_LINES} 行）..."

while IFS= read -r line; do
    count=$(echo "$line" | awk '{print $1}')
    file=$(echo "$line" | awk '{print $2}')
    
    # 忽略特例（如静态 HTML 实体定义查找表）
    if echo "$file" | grep -qE "$EXCLUDE_REGEX"; then
        continue
    fi
    
    if [ "$count" -gt "$MAX_LINES" ]; then
        echo "  [FAIL] $file: $count 行 (超标 $(($count - $MAX_LINES)) 行)"
        FAILED=$((FAILED + 1))
    fi
done < <(find crates/*/src -name "*.rs" -type f -exec wc -l {} + | sort -rn | grep -v ' total$')

echo ""
if [ "$FAILED" -eq 0 ]; then
    echo "✅ 全部文件符合规范（<= ${MAX_LINES} 行）"
    exit 0
else
    echo "⚠️ 共有 $FAILED 个源文件超过 ${MAX_LINES} 行规范，需在后续各专项步骤中按模块分拆。"
    exit 1
fi

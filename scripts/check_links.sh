#!/usr/bin/env bash
# 检查仓库里所有 Markdown 的相对链接：目标文件或目录必须存在。
#   scripts/check_links.sh            # 全部已跟踪的 .md（第三方 vendor 目录除外）
#   scripts/check_links.sh a.md b.md  # 只查指定文件
# 外部链接（http/https/mailto）和纯锚点（#xxx）不查。代码块里的内容会跳过。
# 并发数按核数和可用内存定（scripts/jobs.sh），有失效链接时退出码为 1。
set -uo pipefail
cd "$(dirname "$0")/.."

check_one() {
    local md=$1 dir line target path
    dir=$(dirname "$md")
    # 去掉 ``` 代码块和行内 `code`，再取出 ](目标) 与 [x]: 目标 两种写法
    awk '/^[[:space:]]*```/ {f = !f; next} !f {gsub(/`[^`]*`/, ""); print NR "\t" $0}' "$md" |
    grep -oE '^[0-9]+|\]\([^) ]+|^\[[^]]+\]: +[^ ]+' |
    while IFS= read -r line; do
        case $line in
            [0-9]*) lineno=$line; continue ;;
            "]("*) target=${line#](} ;;
            *) target=${line#*]: } ;;
        esac
        target=${target%%#*}
        target=${target%%\?*}
        case $target in ''|http://*|https://*|mailto:*|data:*|file:*|/*) continue ;; esac
        path=$dir/$target
        [ -e "$path" ] || printf '%s:%s: %s\n' "$md" "${lineno:-?}" "$target"
    done
}
export -f check_one

tmp=$(mktemp)
if [ $# -gt 0 ]; then
    printf '%s\n' "$@"
else
    git ls-files '*.md' | grep -v '/vendor/'
fi | xargs -r -P "$(scripts/jobs.sh 200 1024)" -I{} bash -c 'check_one "$1"' _ {} > "$tmp"
broken=$(sort "$tmp"); rm -f "$tmp"
if [ -n "$broken" ]; then
    echo "$broken"
    echo "失效链接：$(printf '%s\n' "$broken" | wc -l) 处" >&2
    exit 1
fi
echo "Markdown 相对链接全部有效"

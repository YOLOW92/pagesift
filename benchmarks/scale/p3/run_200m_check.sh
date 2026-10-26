#!/usr/bin/env bash
# 200M 本地回归：验证正文定位默认启用 + 表格修复后是否明显掉速。
# 口径与 P3 100M 门禁一致（同一 origin / runner / --formal --split-pipeline）。
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
runner="$repo_root/bench/p0/target/release/p2_http_reactor_runner"
origin="$repo_root/bench/p0/target/release/stream_origin"
tag="${1:-200m}"
result="$repo_root/bench/results/0.70_200m_${tag}.json"
log="$repo_root/bench/results/0.70_200m_${tag}.log"
meta="$repo_root/bench/results/0.70_200m_${tag}.meta.json"
unit="pagesift-200m-origin-${tag}.service"
total_tasks=200000000

for path in "$runner" "$origin"; do
    [[ -x "$path" ]] || { echo "missing binary: $path" >&2; exit 1; }
done
for path in "$result" "$log" "$meta"; do
    [[ -e "$path" ]] && { echo "refusing to overwrite: $path" >&2; exit 1; }
done
pgrep -f '[p]2_http_reactor_runner|[s]tream_origin' >/dev/null && {
    echo "another benchmark process is running" >&2; exit 1; }

cleanup() { systemctl --user stop "$unit" >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

systemd-run --user --unit="${unit%.service}" --collect \
    --property="LimitNOFILE=1048576" \
    --property="WorkingDirectory=$repo_root" \
    "$origin" --bind 127.0.0.1:19800 --total-pages "$total_tasks"

ready=false
for _ in $(seq 1 40); do
    if curl -fsS http://127.0.0.1:19800/stats >/dev/null; then ready=true; break; fi
    sleep 0.25
done
[[ "$ready" == true ]] || { journalctl --user -u "$unit" -n 50 --no-pager >&2; exit 1; }

origin_pid=$(systemctl --user show -p MainPID --value "$unit")
origin_nofile=$(awk '/Max open files/ {print $4}' "/proc/$origin_pid/limits" 2>/dev/null || echo 0)
(( origin_nofile >= 65536 )) || { echo "origin NOFILE too low: $origin_nofile" >&2; exit 1; }

before_stats=$(curl -fsS http://127.0.0.1:19800/stats)
started_ns=$(date +%s%N)
ulimit -n 1048576
set +e
timeout 3600s "$runner" \
    --formal --mode full --split-pipeline \
    --total-tasks "$total_tasks" \
    --telemetry-interval-ms 1000 \
    --output "$result" 2>&1 | tee "$log"
runner_status=${PIPESTATUS[0]}
set -e
finished_ns=$(date +%s%N)
after_stats=$(curl -fsS http://127.0.0.1:19800/stats)

(( runner_status == 0 )) || { echo "runner failed: $runner_status" >&2; exit "$runner_status"; }

wall_s=$(awk -v a="$started_ns" -v b="$finished_ns" 'BEGIN{printf "%.3f", (b-a)/1e9}')
orig_before=$(jq -r '.requests_served' <<<"$before_stats")
orig_after=$(jq -r '.requests_served' <<<"$after_stats")

jq -n \
  --argjson total "$total_tasks" \
  --argjson wall "$wall_s" \
  --argjson before "$orig_before" \
  --argjson after "$orig_after" \
  --slurpfile r "$result" \
  '{schema:1, total_tasks:$total, wall_s:$wall,
    completed:$r[0].completed, valid:$r[0].valid, failed:$r[0].failed,
    sustained_pages_per_s: ($r[0].completed / $wall),
    process_cpu_time_s:$r[0].process_cpu_time_s,
    peak_rss_bytes:$r[0].peak_rss_bytes,
    capacity_adjustments:($r[0].capacity_adjustments|length),
    samples:($r[0].samples|length),
    origin_requests:($after - $before),
    runner_sha: "'"$(sha256sum "$runner" | awk '{print $1}')"'",
    git_head: "'"$(git -C "$repo_root" rev-parse HEAD)"'"}' > "$meta"
cat "$meta"
echo "200M_CHECK_DONE"

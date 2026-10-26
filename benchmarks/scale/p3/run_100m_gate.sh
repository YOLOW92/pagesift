#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# Optional attempt tag keeps a retried formal run from overwriting earlier
# evidence. Usage: run_100m_gate.sh attempt2
attempt="${1:-}"
suffix=""
if [[ -n "$attempt" ]]; then
    if [[ ! "$attempt" =~ ^[A-Za-z0-9._-]+$ ]]; then
        echo "attempt tag may only contain letters, digits, dot, underscore and dash" >&2
        exit 1
    fi
    suffix="_${attempt}"
fi
runner="$repo_root/bench/p0/target/release/p2_http_reactor_runner"
origin="$repo_root/bench/p0/target/release/stream_origin"
result="$repo_root/bench/results/0.70_p3_100m_saturation${suffix}.json"
log="$repo_root/bench/results/0.70_p3_100m_saturation${suffix}.log"
meta="$repo_root/bench/results/0.70_p3_100m_saturation${suffix}.meta.json"
unit="pagesift-p3-origin-final${suffix}.service"
total_tasks=100000000

for path in "$runner" "$origin"; do
    if [[ ! -x "$path" ]]; then
        echo "missing frozen release binary: $path" >&2
        exit 1
    fi
done
for path in "$result" "$log" "$meta"; do
    if [[ -e "$path" ]]; then
        echo "refusing to overwrite formal artifact: $path" >&2
        exit 1
    fi
done
if pgrep -f 'p2_http_reactor_runner|stream_origin' >/dev/null; then
    echo "another PageSift benchmark process is already running" >&2
    exit 1
fi

cleanup() {
    systemctl --user stop "$unit" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

systemd-run --user \
    --unit="${unit%.service}" \
    --collect \
    --property="LimitNOFILE=1048576" \
    --property="WorkingDirectory=$repo_root" \
    "$origin" --bind 127.0.0.1:19800 --total-pages "$total_tasks"

ready=false
for _ in $(seq 1 40); do
    if curl -fsS http://127.0.0.1:19800/stats >/dev/null; then
        ready=true
        break
    fi
    sleep 0.25
done
if [[ "$ready" != true ]]; then
    journalctl --user -u "$unit" -n 100 --no-pager >&2
    exit 1
fi

# A systemd transient unit inherits the manager's soft NOFILE (1024 on this
# host). With more in-flight connections than that, the kernel completes the
# TCP handshake but the origin can never accept the socket, so clients time
# out with an empty response. Refuse to start rather than silently benchmark
# against a starved origin.
origin_pid=$(systemctl --user show -p MainPID --value "$unit")
origin_nofile=$(awk '/Max open files/ {print $4}' "/proc/$origin_pid/limits" 2>/dev/null || echo 0)
if (( origin_nofile < 65536 )); then
    echo "origin soft NOFILE is ${origin_nofile:-unknown}; at least 65536 is required" >&2
    exit 1
fi

before_stats=$(curl -fsS http://127.0.0.1:19800/stats)
started_ns=$(date +%s%N)
ulimit -n 1048576
set +e
timeout 1800s "$runner" \
    --formal \
    --mode full \
    --split-pipeline \
    --total-tasks "$total_tasks" \
    --telemetry-interval-ms 1000 \
    --output "$result" 2>&1 | tee "$log"
runner_status=${PIPESTATUS[0]}
set -e
finished_ns=$(date +%s%N)
after_stats=$(curl -fsS http://127.0.0.1:19800/stats)
if (( runner_status != 0 )); then
    echo "formal runner failed with status $runner_status" >&2
    exit "$runner_status"
fi

expected_samples=$(( (total_tasks + 9999) / 10000 ))
jq -e \
    --argjson tasks "$total_tasks" \
    --argjson samples "$expected_samples" '
    . as $r |
    $r.schema == 5 and
    $r.formal == true and
    $r.total_tasks == $tasks and
    $r.completed == $tasks and
    $r.valid == $tasks and
    $r.failed == 0 and
    ($r.samples | length) == $samples and
    ([$r.samples[].task_id] | unique | length) == $samples and
    ([$r.node_reports[].completed] | add) == $tasks and
    ([$r.node_reports[].failed] | add) == 0 and
    $r.process_cpu_time_s > 0 and
    $r.peak_rss_bytes < 2147483648 and
    ($r.capacity_adjustments | length) > 0
    ' "$result" >/dev/null

before_requests=$(jq -r '.requests_served' <<<"$before_stats")
after_requests=$(jq -r '.requests_served' <<<"$after_stats")
origin_delta=$((after_requests - before_requests))
if (( origin_delta < total_tasks )); then
    echo "origin served only $origin_delta requests for $total_tasks completed tasks" >&2
    exit 1
fi

runner_sha=$(sha256sum "$runner" | awk '{print $1}')
origin_sha=$(sha256sum "$origin" | awk '{print $1}')
result_sha=$(sha256sum "$result" | awk '{print $1}')
log_sha=$(sha256sum "$log" | awk '{print $1}')
source_sha=$(sha256sum "$repo_root/bench/p0/src/p2_http_reactor_runner.rs" | awk '{print $1}')
lock_sha=$(sha256sum "$repo_root/bench/p0/Cargo.lock" | awk '{print $1}')
git_head=$(git -C "$repo_root" rev-parse HEAD)
git_diff_sha=$(git -C "$repo_root" diff --binary | sha256sum | awk '{print $1}')
kernel=$(uname -srvmo)
cpu_max=$(cat /sys/fs/cgroup/cpu.max 2>/dev/null || true)
memory_max=$(cat /sys/fs/cgroup/memory.max 2>/dev/null || true)
wall_ns=$((finished_ns - started_ns))

jq -n \
    --argjson schema 1 \
    --arg gate "PageSift 0.70 P3 frozen 100M saturation gate" \
    --arg attempt "$attempt" \
    --argjson total_tasks "$total_tasks" \
    --argjson started_ns "$started_ns" \
    --argjson finished_ns "$finished_ns" \
    --argjson wall_ns "$wall_ns" \
    --argjson origin_requests "$origin_delta" \
    --arg runner_sha256 "$runner_sha" \
    --arg origin_sha256 "$origin_sha" \
    --arg source_sha256 "$source_sha" \
    --arg cargo_lock_sha256 "$lock_sha" \
    --arg result_sha256 "$result_sha" \
    --arg log_sha256 "$log_sha" \
    --arg git_head "$git_head" \
    --arg git_diff_sha256 "$git_diff_sha" \
    --arg kernel "$kernel" \
    --arg cpu_max "$cpu_max" \
    --arg memory_max "$memory_max" \
    --argjson origin_nofile "$origin_nofile" \
    --argjson before_stats "$before_stats" \
    --argjson after_stats "$after_stats" '
    {
      schema: $schema,
      gate: $gate,
      attempt: $attempt,
      contract: {
        total_tasks: $total_tasks,
        mode: "full",
        split_pipeline: true,
        manual_reactors: false,
        manual_connections: false,
        fixture_pages: 5,
        sample_every: 10000
      },
      timing: {started_ns: $started_ns, finished_ns: $finished_ns, wall_ns: $wall_ns},
      origin: {requests_during_gate: $origin_requests, before: $before_stats, after: $after_stats, soft_nofile: $origin_nofile},
      provenance: {
        runner_sha256: $runner_sha256,
        origin_sha256: $origin_sha256,
        runner_source_sha256: $source_sha256,
        cargo_lock_sha256: $cargo_lock_sha256,
        git_head: $git_head,
        git_diff_sha256: $git_diff_sha256,
        kernel: $kernel,
        cgroup_cpu_max: $cpu_max,
        cgroup_memory_max: $memory_max
      },
      artifacts: {result_sha256: $result_sha256, log_sha256: $log_sha256}
    }' >"$meta"

echo "P3 formal 100M gate passed"
sha256sum "$result" "$log" "$meta"

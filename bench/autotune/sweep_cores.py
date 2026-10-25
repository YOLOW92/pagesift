#!/usr/bin/env python3
"""PageSift 0.70 P1: reproducible 1-to-192 CPU scaling sweep and USL fit."""

from __future__ import annotations

import argparse
import csv
import functools
import json
import math
import os
import random
import re
import resource
import shutil
import statistics
import subprocess
import time
import urllib.request
from collections import defaultdict
from pathlib import Path

import numpy as np
from scipy.optimize import curve_fit


ROOT = Path(__file__).resolve().parents[2]
BENCH = ROOT / "bench" / "p0"
TARGET = BENCH / "target" / "release"
ORIGIN = TARGET / "stream_origin"
RUNNER = TARGET / "sharded_runner"
RESULTS = ROOT / "bench" / "results" / "scaling_cores_192.csv"
PARTIAL = RESULTS.with_suffix(".partial.csv")
METADATA = ROOT / "bench" / "results" / "scaling_cores_192.meta.json"
REPORT = ROOT / "docs" / "benchmarks" / "0.70_p1_cpu_scaling_usl.md"

CORE_LADDER = [
    1, 2, 4, 6, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64,
    72, 80, 88, 96, 104, 112, 120, 128,
    144, 160, 176, 184, 192,
]
ALPHAS = [4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 64]
FIELDS = [
    "cores",
    "topology",
    "shards",
    "alpha",
    "conc_per_shard",
    "total_concurrency",
    "repeat",
    "tasks",
    "qps",
    "elapsed_s",
    "cpu_util_pct",
    "process_cpu_time_s",
    "rss_peak_mb",
    "queue_p50_ms",
    "queue_p95_ms",
    "queue_p99_ms",
    "queue_samples",
    "failed",
]

TOPOLOGY_PROBES = {
    "numa_spread": [24, 48, 72, 96, 128, 160],
    "single_socket": [24, 48, 72, 96],
    "single_numa": [12, 24, 36, 48],
}


def parse_cpu_list(value: str) -> list[int]:
    cpus = []
    for part in value.strip().split(","):
        if "-" in part:
            start, end = (int(item) for item in part.split("-", 1))
            cpus.extend(range(start, end + 1))
        elif part:
            cpus.append(int(part))
    return cpus


def round_robin(groups: list[list[int]]) -> list[int]:
    result = []
    for index in range(max((len(group) for group in groups), default=0)):
        result.extend(group[index] for group in groups if index < len(group))
    return result


@functools.lru_cache(maxsize=1)
def topology_orders() -> dict[str, list[int]]:
    online = parse_cpu_list(Path("/sys/devices/system/cpu/online").read_text())
    package = {
        cpu: int(
            Path(f"/sys/devices/system/cpu/cpu{cpu}/topology/physical_package_id")
            .read_text()
            .strip()
        )
        for cpu in online
    }
    core_key = {
        cpu: (
            package[cpu],
            int(
                Path(f"/sys/devices/system/cpu/cpu{cpu}/topology/core_id")
                .read_text()
                .strip()
            ),
        )
        for cpu in online
    }
    physical = []
    seen = set()
    for cpu in online:
        if core_key[cpu] not in seen:
            physical.append(cpu)
            seen.add(core_key[cpu])
    siblings = [cpu for cpu in online if cpu not in set(physical)]

    nodes = []
    for path in sorted(Path("/sys/devices/system/node").glob("node[0-9]*")):
        node_cpus = set(parse_cpu_list((path / "cpulist").read_text()))
        nodes.append(
            [cpu for cpu in physical if cpu in node_cpus]
            + [cpu for cpu in siblings if cpu in node_cpus]
        )
    node_physical = [[cpu for cpu in node if cpu in set(physical)] for node in nodes]
    node_siblings = [[cpu for cpu in node if cpu in set(siblings)] for node in nodes]

    first_package = min(package.values())
    socket_physical = [cpu for cpu in physical if package[cpu] == first_package]
    socket_siblings = [cpu for cpu in siblings if package[cpu] == first_package]
    return {
        "compact": physical + siblings,
        "numa_spread": round_robin(node_physical) + round_robin(node_siblings),
        "single_socket": socket_physical + socket_siblings,
        "single_numa": nodes[0] if nodes else physical + siblings,
    }


def cpu_mask(cores: int, topology: str) -> str:
    order = topology_orders()[topology]
    if cores > len(order):
        raise ValueError(f"{topology} exposes only {len(order)} CPUs, requested {cores}")
    return ",".join(str(cpu) for cpu in order[:cores])


def shard_candidates(cores: int) -> list[int]:
    return sorted(
        {
            1,
            min(cores, 2),
            max(1, math.ceil(cores / 16)),
            max(1, math.ceil(cores / 12)),
            max(1, math.ceil(cores / 8)),
            max(1, math.ceil(cores / 6)),
            max(1, math.ceil(cores / 4)),
            max(1, math.ceil(cores / 3)),
            max(1, math.ceil(cores / 2)),
            min(cores, 48),
            min(cores, 64),
            min(cores, 96),
        }
    )


def wait_for_origin(url: str, attempts: int = 100) -> None:
    for _ in range(attempts):
        try:
            with urllib.request.urlopen(f"{url}/stats", timeout=1) as response:
                if response.status == 200:
                    return
        except Exception:
            time.sleep(0.1)
    raise RuntimeError(f"origin did not become ready: {url}")


def parse_summary(stdout: str) -> dict[str, float | int]:
    patterns = {
        "qps": r"Sustained QPS:\s+([0-9.]+)",
        "elapsed_s": r"Wall Clock Time:\s+([0-9.]+)s",
        "failed": r"Failed Count:\s+(\d+)",
        "queue": (
            r"Queue Latency:\s+p50 ([0-9.]+) ms \| p95 ([0-9.]+) ms "
            r"\| p99 ([0-9.]+) ms \((\d+) samples\)"
        ),
        "cpu": (
            r"Runner CPU Time:\s+([0-9.]+) s \| Utilization ([0-9.]+)% "
            r"of (\d+) assigned cores"
        ),
    }
    qps = re.search(patterns["qps"], stdout)
    elapsed = re.search(patterns["elapsed_s"], stdout)
    failed = re.search(patterns["failed"], stdout)
    queue = re.search(patterns["queue"], stdout)
    cpu = re.search(patterns["cpu"], stdout)
    if not all((qps, elapsed, failed, queue, cpu)):
        raise RuntimeError(f"incomplete runner summary:\n{stdout[-3000:]}")
    return {
        "qps": float(qps.group(1)),
        "elapsed_s": float(elapsed.group(1)),
        "failed": int(failed.group(1)),
        "queue_p50_ms": float(queue.group(1)),
        "queue_p95_ms": float(queue.group(2)),
        "queue_p99_ms": float(queue.group(3)),
        "queue_samples": int(queue.group(4)),
        "process_cpu_time_s": float(cpu.group(1)),
        "cpu_util_pct": float(cpu.group(2)),
        "assigned_cores": int(cpu.group(3)),
    }


def parse_telemetry(path: Path) -> float:
    rows = []
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    peak_rss = max((float(row.get("peak_rss_mb", 0.0)) for row in rows), default=0.0)
    return peak_rss


def run_point(
    cores: int,
    topology: str,
    shards: int,
    alpha: int,
    repeat: int,
    tasks: int,
    port: int,
) -> dict[str, float | int]:
    concurrency = cores * alpha
    conc_per_shard = max(1, math.ceil(concurrency / shards))
    mask = cpu_mask(cores, topology)
    origin_url = f"http://127.0.0.1:{port}"
    scratch = BENCH / ".p1_tmp" / f"{cores}c-{topology}-{shards}s-{alpha}a-r{repeat}"
    samples = scratch / "samples"
    telemetry = scratch / "telemetry.jsonl"
    if scratch.exists():
        shutil.rmtree(scratch)
    samples.mkdir(parents=True)

    origin = subprocess.Popen(
        [
            "taskset",
            "-c",
            mask,
            str(ORIGIN),
            "--bind",
            f"127.0.0.1:{port}",
            "--total-pages",
            str(tasks),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait_for_origin(origin_url)
        command = [
            "taskset",
            "-c",
            mask,
            str(RUNNER),
            "--origin-url",
            origin_url,
            "--total-tasks",
            str(tasks),
            "--shards",
            str(shards),
            "--concurrency-per-shard",
            str(conc_per_shard),
            "--sample-dir",
            str(samples),
            "--sample-every",
            str(tasks + 1),
            "--queue-sample-every",
            "256",
            "--telemetry-interval-ms",
            "100",
            "--report-file",
            str(telemetry),
        ]
        completed = subprocess.run(command, capture_output=True, text=True)
        if completed.returncode != 0:
            raise RuntimeError(
                f"runner exited {completed.returncode}:\n{completed.stderr[-3000:]}"
            )
        summary = parse_summary(completed.stdout)
        peak_rss = parse_telemetry(telemetry)
        return {
            "cores": cores,
            "topology": topology,
            "shards": shards,
            "alpha": alpha,
            "conc_per_shard": conc_per_shard,
            "total_concurrency": shards * conc_per_shard,
            "repeat": repeat,
            "tasks": tasks,
            "qps": summary["qps"],
            "elapsed_s": summary["elapsed_s"],
            "cpu_util_pct": summary["cpu_util_pct"],
            "process_cpu_time_s": summary["process_cpu_time_s"],
            "rss_peak_mb": round(peak_rss, 3),
            "queue_p50_ms": summary["queue_p50_ms"],
            "queue_p95_ms": summary["queue_p95_ms"],
            "queue_p99_ms": summary["queue_p99_ms"],
            "queue_samples": summary["queue_samples"],
            "failed": summary["failed"],
        }
    finally:
        origin.terminate()
        try:
            origin.wait(timeout=3)
        except subprocess.TimeoutExpired:
            origin.kill()
            origin.wait(timeout=3)
        shutil.rmtree(scratch, ignore_errors=True)


def median_points(rows: list[dict[str, float | int]]) -> list[dict[str, float | int]]:
    groups: dict[tuple[int, str, int, int], list[dict[str, float | int]]] = defaultdict(list)
    for row in rows:
        groups[
            (
                int(row["cores"]),
                str(row["topology"]),
                int(row["shards"]),
                int(row["alpha"]),
            )
        ].append(row)
    medians = []
    for (cores, topology, shards, alpha), samples in groups.items():
        medians.append(
            {
                "cores": cores,
                "topology": topology,
                "shards": shards,
                "alpha": alpha,
                "conc_per_shard": int(samples[0]["conc_per_shard"]),
                "total_concurrency": int(samples[0]["total_concurrency"]),
                **{
                    field: statistics.median(float(sample[field]) for sample in samples)
                    for field in (
                        "qps",
                        "elapsed_s",
                        "cpu_util_pct",
                        "process_cpu_time_s",
                        "rss_peak_mb",
                        "queue_p50_ms",
                        "queue_p95_ms",
                        "queue_p99_ms",
                    )
                },
                "failed": max(int(sample["failed"]) for sample in samples),
            }
        )
    return medians


def usl(n: np.ndarray | float, gamma: float, sigma: float, kappa: float):
    n = np.asarray(n, dtype=float)
    return gamma * n / (1.0 + sigma * (n - 1.0) + kappa * n * (n - 1.0))


def fit_usl(best: dict[int, dict[str, float | int]]) -> dict[str, float]:
    x = np.asarray(sorted(best), dtype=float)
    y = np.asarray([float(best[int(core)]["qps"]) for core in x], dtype=float)
    params, _ = curve_fit(
        usl,
        x,
        y,
        p0=[y[0], 0.01, 0.0001],
        bounds=([1.0, 0.0, 0.0], [1_000_000.0, 1.0, 0.1]),
        maxfev=50_000,
    )
    predicted = usl(x, *params)
    residual = float(np.sum((y - predicted) ** 2))
    total = float(np.sum((y - np.mean(y)) ** 2))
    gamma, sigma, kappa = (float(value) for value in params)
    n_peak = math.sqrt((1.0 - sigma) / kappa) if kappa > 0 and sigma < 1 else math.inf
    return {
        "gamma": gamma,
        "sigma": sigma,
        "kappa": kappa,
        "r_squared": 1.0 - residual / total if total > 0 else 1.0,
        "n_peak": n_peak,
        "qps_peak": float(usl(n_peak, *params)) if math.isfinite(n_peak) else math.inf,
    }


def write_report(
    best: dict[int, dict[str, float | int]],
    topology_rows: list[dict[str, float | int]],
    fit: dict[str, float],
    confirm_top: int,
    confirm_repeats: int,
) -> None:
    base = float(best[1]["qps"])
    table = []
    for cores in sorted(best):
        row = best[cores]
        qps = float(row["qps"])
        table.append(
            f"| {cores} | {row['shards']} | {row['alpha']} | {row['total_concurrency']} | "
            f"{qps:,.1f} | {qps / base:.2f}x | {qps / (base * cores) * 100:.1f}% | "
            f"{float(row['cpu_util_pct']):.1f}% | {float(row['process_cpu_time_s']):.3f} | "
            f"{float(row['rss_peak_mb']):.1f} | "
            f"{float(row['queue_p50_ms']):.3f} / {float(row['queue_p95_ms']):.3f} / "
            f"{float(row['queue_p99_ms']):.3f} |"
        )
    topology_table = []
    for row in sorted(
        (row for row in topology_rows if row["topology"] != "compact"),
        key=lambda row: (int(row["cores"]), str(row["topology"])),
    ):
        cores = int(row["cores"])
        qps = float(row["qps"])
        compact_qps = float(best[cores]["qps"])
        topology_table.append(
            f"| {cores} | {row['topology']} | {qps:,.1f} | "
            f"{(qps / compact_qps - 1.0) * 100:+.2f}% | "
            f"{float(row['cpu_util_pct']):.1f}% | {float(row['process_cpu_time_s']):.3f} | "
            f"{float(row['queue_p99_ms']):.3f} |"
        )
    generated = time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime())
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(
        f"""# PageSift 0.70 P1：1C 至 192C 全阶梯实测与 USL 可扩展性定律

> 环境：Google Cloud `c4-highcpu-192`，192 vCPU，4 NUMA 节点  
> 生成时间：{generated}  
> 原始数据：[`bench/results/scaling_cores_192.csv`](../../bench/results/scaling_cores_192.csv)

## 方法

- 阶梯：`{CORE_LADDER}`；每档扫描并发比 `alpha={ALPHAS}` 与 1/8、1/4、1/2 核心比例等分片粒度。
- 合成源站与 PageSift runner 同时绑定到相同 CPU 集合；96 线程前优先使用物理核，之后加入 SMT sibling。
- 第一阶段对完整扩展网格各实测一次；第二阶段对每个核心档前 {confirm_top} 名配置补测至 {confirm_repeats} 次并取中位数。
- 单次任务量随核心数增长，避免高核点短跑失真。
- CPU 指标只使用 Rust Runner 自身 `/proc/self/stat` 的 `utime + stime`，不包含合成源站、其他进程或系统 CPU 时间，并按 Runner 的 CPU 亲和性容量归一化。
- 完成前写入 `.partial.csv`，全部点结束后才原子替换正式 CSV；不稳定点保留失败数并从最优候选中排除。

## 各核心档位最优实测点

| vCPU | shards | alpha | concurrency | QPS | speedup | efficiency | Runner CPU | CPU time s | peak RSS MB | queue p50/p95/p99 ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
{chr(10).join(table)}

## USL 拟合

`QPS(N) = gamma*N / (1 + sigma*(N-1) + kappa*N*(N-1))`

- gamma = **{fit['gamma']:.3f} pages/s/core**
- sigma = **{fit['sigma']:.8f}**（{fit['sigma'] * 100:.5f}%）
- kappa = **{fit['kappa']:.10f}**（{fit['kappa'] * 100:.7f}%）
- R² = **{fit['r_squared']:.6f}**
- 理论拐点 N* = **{fit['n_peak']:.2f} vCPU**，QPS* = **{fit['qps_peak']:,.1f} pages/s**

## 双路 Xeon 拓扑对照

`compact` 是 physical-first 主基线；下表把相同核心数、相同分片与并发配置放到单 NUMA、单 Socket 或跨 NUMA 均匀分布的 CPU 集合上，差值均相对主基线。

| vCPU | placement | QPS | vs compact | Runner CPU | CPU time s | queue p99 ms |
|---:|:---|---:|---:|---:|---:|---:|
{chr(10).join(topology_table)}
""",
        encoding="utf-8",
    )


def load_partial() -> list[dict[str, float | int]]:
    if not PARTIAL.exists():
        return []
    rows = []
    with PARTIAL.open(newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            rows.append(
                {
                    field: value
                    if field == "topology"
                    else int(value)
                    if field
                    in {
                        "cores",
                        "shards",
                        "alpha",
                        "conc_per_shard",
                        "total_concurrency",
                        "repeat",
                        "tasks",
                        "queue_samples",
                        "failed",
                    }
                    else float(value)
                    for field, value in row.items()
                }
            )
    return rows


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--min-tasks", type=int, default=100_000)
    parser.add_argument("--tasks-per-core", type=int, default=10_000)
    parser.add_argument("--confirm-top", type=int, default=3)
    parser.add_argument("--confirm-repeats", type=int, default=3)
    parser.add_argument("--port", type=int, default=19_850)
    parser.add_argument("--fresh", action="store_true")
    args = parser.parse_args()

    host_cpus = os.cpu_count() or 1
    if host_cpus < max(CORE_LADDER):
        raise SystemExit(f"P1 requires 192 visible CPUs; found {host_cpus}")
    if not ORIGIN.exists() or not RUNNER.exists():
        raise SystemExit("release benchmark binaries are missing")
    try:
        resource.setrlimit(resource.RLIMIT_NOFILE, (1_048_576, 1_048_576))
    except (ValueError, OSError):
        pass

    RESULTS.parent.mkdir(parents=True, exist_ok=True)
    if args.fresh and PARTIAL.exists():
        PARTIAL.unlink()
    rows = load_partial()
    completed = {
        (
            int(row["cores"]),
            str(row["topology"]),
            int(row["shards"]),
            int(row["alpha"]),
            int(row["repeat"]),
        )
        for row in rows
    }
    new_file = not PARTIAL.exists()
    with PARTIAL.open("a", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDS)
        if new_file:
            writer.writeheader()
            handle.flush()
        def measure(
            cores: int,
            topology: str,
            shards: int,
            alpha: int,
            repeat: int,
            phase: str,
        ) -> None:
            key = (cores, topology, shards, alpha, repeat)
            if key in completed:
                return
            tasks = max(args.min_tasks, cores * args.tasks_per_core)
            print(
                f"{phase} cores={cores} topology={topology} shards={shards} alpha={alpha} "
                f"repeat={repeat} tasks={tasks}",
                flush=True,
            )
            row = run_point(
                cores, topology, shards, alpha, repeat, tasks, args.port
            )
            writer.writerow(row)
            handle.flush()
            os.fsync(handle.fileno())
            rows.append(row)
            completed.add(key)
            print(
                f"OK qps={float(row['qps']):,.1f} cpu={float(row['cpu_util_pct']):.1f}% "
                f"queue_p99={float(row['queue_p99_ms']):.3f}ms failed={int(row['failed'])}",
                flush=True,
            )

        # Phase A: one full expanded-grid observation per configuration.
        for cores in CORE_LADDER:
            candidates = [(shards, alpha) for shards in shard_candidates(cores) for alpha in ALPHAS]
            random.Random(cores).shuffle(candidates)
            for shards, alpha in candidates:
                measure(cores, "compact", shards, alpha, 1, "GRID")

        # Phase B: confirm each core tier's strongest coarse candidates.
        confirmed: dict[int, list[tuple[int, int]]] = {}
        coarse = median_points(
            [
                row for row in rows
                if int(row["repeat"]) == 1 and row["topology"] == "compact"
            ]
        )
        for cores in CORE_LADDER:
            ranked = sorted(
                (row for row in coarse if int(row["cores"]) == cores),
                key=lambda row: float(row["qps"]),
                reverse=True,
            )
            ranked = [row for row in ranked if int(row["failed"]) == 0][
                : args.confirm_top
            ]
            if len(ranked) < args.confirm_top:
                raise RuntimeError(
                    f"only {len(ranked)} stable coarse candidates at {cores} cores"
                )
            confirmed[cores] = [
                (int(row["shards"]), int(row["alpha"])) for row in ranked
            ]
            for shards, alpha in confirmed[cores]:
                for repeat in range(2, args.confirm_repeats + 1):
                    measure(cores, "compact", shards, alpha, repeat, "CONFIRM")

        # Phase C: isolate socket, NUMA and SMT effects using the best compact config.
        compact_medians = median_points(rows)
        compact_best = {
            cores: max(
                (
                    row for row in compact_medians
                    if int(row["cores"]) == cores
                    and row["topology"] == "compact"
                    and int(row["failed"]) == 0
                    and (int(row["shards"]), int(row["alpha"])) in confirmed[cores]
                ),
                key=lambda row: float(row["qps"]),
            )
            for cores in CORE_LADDER
        }
        for topology, core_tiers in TOPOLOGY_PROBES.items():
            for cores in core_tiers:
                config = compact_best[cores]
                for repeat in range(1, args.confirm_repeats + 1):
                    measure(
                        cores,
                        topology,
                        int(config["shards"]),
                        int(config["alpha"]),
                        repeat,
                        "TOPOLOGY",
                    )

    medians = median_points(rows)
    best = {
        cores: max(
            (
                row for row in medians
                if int(row["cores"]) == cores
                and row["topology"] == "compact"
                and int(row["failed"]) == 0
                and (int(row["shards"]), int(row["alpha"])) in confirmed[cores]
            ),
            key=lambda row: float(row["qps"]),
        )
        for cores in CORE_LADDER
    }
    fit = fit_usl(best)
    PARTIAL.replace(RESULTS)
    write_report(best, medians, fit, args.confirm_top, args.confirm_repeats)
    METADATA.write_text(
        json.dumps(
            {
                "generated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "core_ladder": CORE_LADDER,
                "alphas": ALPHAS,
                "topology_probes": TOPOLOGY_PROBES,
                "topology_orders": topology_orders(),
                "confirm_top": args.confirm_top,
                "confirm_repeats": args.confirm_repeats,
                "min_tasks": args.min_tasks,
                "tasks_per_core": args.tasks_per_core,
                "host": subprocess.check_output(["uname", "-a"], text=True).strip(),
                "git_head": subprocess.check_output(
                    ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
                ).strip(),
                "fit": fit,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    print(f"COMPLETE results={RESULTS} report={REPORT}", flush=True)


if __name__ == "__main__":
    main()

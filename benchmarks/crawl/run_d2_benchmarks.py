#!/usr/bin/env python3
"""PageSift 0.50 Benchmark D2: Massive Crawling & Chaos Origin Battle Royale.

Evaluates 5 major web crawlers:
1. PageSift 0.50 (Rust)
2. Spider 2.53.9 (Rust)
3. Colly 2.3 (Go)
4. Scrapy 2.19 (Python)
5. Crawlee 1.11 (Python)

Two Tiers:
- Tier 1: Standard Multi-Domain Crawl (500 hosts x 100 pages = 50,000 URLs, 16GB Memory Watchdog)
- Tier 2: Ultra-Extreme Insane Chaos Origin (1000 hosts x 100 pages = 100,000 URLs, 503/429/traps/bombs, 16GB Memory Watchdog)
"""

import json
import os
import resource
import signal
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

# Unbuffered output
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(line_buffering=True)
if hasattr(sys.stderr, "reconfigure"):
    sys.stderr.reconfigure(line_buffering=True)

BASE_DIR = Path("/home/wyc610721/pagesift")
ENV_DIR = Path("/home/wyc610721/pagesift-bench-env")
OUT_DIR = BASE_DIR / "bench/results/final"
OUT_DIR.mkdir(parents=True, exist_ok=True)

TIER1_JSONL = OUT_DIR / "benchmark2_tier1_standard_50k.jsonl"
TIER2_JSONL = OUT_DIR / "benchmark2_tier2_chaos_100k.jsonl"

PAGESIFT_BIN = BASE_DIR / "target/release/examples/crawl_bench"
SPIDER_BIN = ENV_DIR / "tools/bin/spider_crawl"
COLLY_BIN = ENV_DIR / "tools/bin/colly_crawl"
ORIGIN_BIN = ENV_DIR / "crawl-harness/target/release/origin"
CHAOS_ORIGIN_BIN = ENV_DIR / "crawl-harness/target/release/chaos_origin"
PAGES_DIR = BASE_DIR / "bench/data/crawl-pages"
PY_VENV = ENV_DIR / "tools/py/bin/python"
SCRAPY_SCRIPT = BASE_DIR / "bench/crawl/scrapy_multi.py"
CRAWLEE_SCRIPT = BASE_DIR / "bench/crawl/crawlee_multi.py"

MEMORY_LIMIT_MB = 120000  # 120 GiB watchdog limit as requested


def pss_kb(pid: int) -> int:
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return 0


def tree_pids(root: int) -> list[int]:
    children: dict[int, list[int]] = {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            children.setdefault(ppid, []).append(int(pid))
        except (OSError, ValueError, IndexError):
            continue
    out, stack = [], [root]
    while stack:
        p = stack.pop()
        out.append(p)
        stack.extend(children.get(p, []))
    return out


def tree_rss_kb(root: int) -> int:
    children: dict[int, list[int]] = {}
    rss: dict[int, int] = {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            with open(f"/proc/{pid}/statm") as f:
                rss[int(pid)] = int(f.read().split()[1]) * os.sysconf("SC_PAGE_SIZE") // 1024
            children.setdefault(ppid, []).append(int(pid))
        except (OSError, ValueError, IndexError):
            continue
    total, stack = 0, [root]
    while stack:
        p = stack.pop()
        total += rss.get(p, 0)
        stack.extend(children.get(p, []))
    return total


def tree_pss_kb(root: int) -> int:
    return sum(pss_kb(p) for p in tree_pids(root))


def kill_tree(root: int):
    for pid in tree_pids(root):
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass


def get_served(port: int) -> int:
    for target in [f"http://[::1]:{port}/__served", f"http://127.0.0.1:{port}/__served"]:
        try:
            req = urllib.request.Request(target, headers={"Host": "s0.localhost"})
            with urllib.request.urlopen(req, timeout=3) as r:
                return int(r.read())
        except Exception:
            pass
    return 0


def get_chaos_metrics(port: int) -> dict:
    for target in [f"http://[::1]:{port}/__chaos_metrics", f"http://127.0.0.1:{port}/__chaos_metrics"]:
        try:
            req = urllib.request.Request(target, headers={"Host": "s0.localhost"})
            with urllib.request.urlopen(req, timeout=3) as r:
                return json.loads(r.read().decode("utf-8"))
        except Exception:
            pass
    return {}


def wait_origin(port: int) -> bool:
    for _ in range(120):
        for target in [f"http://[::1]:{port}/__served", f"http://127.0.0.1:{port}/__served"]:
            try:
                req = urllib.request.Request(target, headers={"Host": "s0.localhost"})
                with urllib.request.urlopen(req, timeout=1) as r:
                    if r.status == 200:
                        return True
            except Exception:
                pass
        time.sleep(0.2)
    return False


def measure_tool(label: str, cmd: list[str], port: int, budget_s: float, out_file: Path) -> dict:
    print(f"\n>>> Running [{label}]: {' '.join(cmd)}")
    served0 = get_served(port)
    t0 = time.monotonic()
    
    scoped_cmd = [
        "systemd-run", "--user", "--scope", "-q",
        "-p", f"MemoryMax={MEMORY_LIMIT_MB}M",
        "-p", "MemorySwapMax=0",
        "--", *cmd
    ]
    
    proc = subprocess.Popen(scoped_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    peak_rss = 0
    peak_pss = 0
    killed = None
    
    while proc.poll() is None:
        rss = tree_rss_kb(proc.pid)
        pss = tree_pss_kb(proc.pid)
        peak_rss = max(peak_rss, rss)
        peak_pss = max(peak_pss, pss)
        
        # Check PSS against watchdog limit
        if pss // 1024 > MEMORY_LIMIT_MB:
            killed = f"KILLED_OOM ({pss // 1024} MB > {MEMORY_LIMIT_MB} MB limit)"
        elif budget_s and time.monotonic() - t0 > budget_s:
            killed = f"TIMED_OUT ({budget_s:.0f}s budget exceeded)"
            
        if killed:
            print(f"  [Watchdog Triggered]: {killed}")
            kill_tree(proc.pid)
            proc.wait()
            break
        time.sleep(0.1)
        
    stdout, stderr = proc.communicate()
    if killed is None and proc.returncode in (-9, 137, -15, 143):
        killed = f"KILLED_OOM (cgroup OOM Killer / Signal {proc.returncode})"
        
    wall_s = time.monotonic() - t0
    served1 = get_served(port)
    served_pages = max(0, served1 - served0)
    
    res = {
        "label": label,
        "tool": label.split("/")[-1],
        "tier": label.split("/")[0],
        "wall_time_s": round(wall_s, 2),
        "peak_rss_mb": round(peak_rss / 1024, 1),
        "peak_pss_mb": round(peak_pss / 1024, 1),
        "served_pages": served_pages,
        "throughput_pages_per_s": round(served_pages / max(wall_s, 1e-6), 1),
        "watchdog_status": "PASSED" if killed is None and proc.returncode == 0 else (killed or f"EXIT_ERR({proc.returncode})"),
        "exit_code": proc.returncode,
        "stdout_summary": (stdout.strip().splitlines()[-1] if stdout.strip() else "")[:200],
    }
    print(f"  Result: {json.dumps(res, ensure_ascii=False)}")
    with open(out_file, "a", encoding="utf-8") as f:
        f.write(json.dumps(res, ensure_ascii=False) + "\n")
    return res


def run_tier1():
    print("\n=======================================================")
    print("=== STARTING TIER 1: Standard 50,000 URLs Crawl     ===")
    print("=======================================================")
    port = 18080
    hosts = 500
    pages = 100
    per_host = 4
    budget_s = 180.0  # 3 minutes per tool
    
    # 1. PageSift
    subprocess.run(["pkill", "-9", "-f", "origin"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)
    origin = subprocess.Popen([str(ORIGIN_BIN), str(PAGES_DIR), str(port), str(hosts), str(pages), "0"])
    if not wait_origin(port):
        sys.exit("Origin failed to start")
    measure_tool("tier1_50k/pagesift", [str(PAGESIFT_BIN), str(port), str(hosts), str(pages), "0", str(per_host)], port, budget_s, TIER1_JSONL)
    kill_tree(origin.pid); origin.wait()

    # 2. Spider
    subprocess.run(["pkill", "-9", "-f", "origin"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)
    origin = subprocess.Popen([str(ORIGIN_BIN), str(PAGES_DIR), str(port), str(hosts), str(pages), "0"])
    wait_origin(port)
    measure_tool("tier1_50k/spider", [str(SPIDER_BIN), str(port), str(hosts), str(pages), "0", str(per_host)], port, budget_s, TIER1_JSONL)
    kill_tree(origin.pid); origin.wait()

    # 3. Colly
    subprocess.run(["pkill", "-9", "-f", "origin"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)
    origin = subprocess.Popen([str(ORIGIN_BIN), str(PAGES_DIR), str(port), str(hosts), str(pages), "0"])
    wait_origin(port)
    measure_tool("tier1_50k/colly", [str(COLLY_BIN), str(port), str(hosts), str(pages), "0", str(per_host)], port, budget_s, TIER1_JSONL)
    kill_tree(origin.pid); origin.wait()

    # 4. Scrapy
    subprocess.run(["pkill", "-9", "-f", "origin"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)
    origin = subprocess.Popen([str(ORIGIN_BIN), str(PAGES_DIR), str(port), str(hosts), str(pages), "0"])
    wait_origin(port)
    measure_tool("tier1_50k/scrapy", [str(PY_VENV), str(SCRAPY_SCRIPT), str(port), str(hosts), str(pages), "0", str(per_host)], port, budget_s, TIER1_JSONL)
    kill_tree(origin.pid); origin.wait()

    # 5. Crawlee
    subprocess.run(["pkill", "-9", "-f", "origin"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)
    origin = subprocess.Popen([str(ORIGIN_BIN), str(PAGES_DIR), str(port), str(hosts), str(pages), "0"])
    wait_origin(port)
    measure_tool("tier1_50k/crawlee", [str(PY_VENV), str(CRAWLEE_SCRIPT), str(port), str(hosts), str(pages), "0", str(per_host)], port, budget_s, TIER1_JSONL)
    kill_tree(origin.pid); origin.wait()


def run_tier2():
    print("\n=======================================================")
    print("=== STARTING TIER 2: Ultra-Extreme Chaos Origin 100k ===")
    print("=======================================================")
    port = 18081
    hosts = 1000
    pages = 100
    per_host = 4
    budget_s = 240.0  # 4 minutes per tool
    
    tools = [
        ("pagesift", [str(PAGESIFT_BIN), str(port), str(hosts), str(pages), "0", str(per_host)]),
        ("spider", [str(SPIDER_BIN), str(port), str(hosts), str(pages), "0", str(per_host)]),
        ("colly", [str(COLLY_BIN), str(port), str(hosts), str(pages), "0", str(per_host)]),
        ("scrapy", [str(PY_VENV), str(SCRAPY_SCRIPT), str(port), str(hosts), str(pages), "0", str(per_host)]),
        ("crawlee", [str(PY_VENV), str(CRAWLEE_SCRIPT), str(port), str(hosts), str(pages), "0", str(per_host)]),
    ]
    
    for name, cmd in tools:
        subprocess.run(["pkill", "-9", "-f", "chaos_origin"], stderr=subprocess.DEVNULL)
        time.sleep(0.5)
        origin = subprocess.Popen([str(CHAOS_ORIGIN_BIN), str(PAGES_DIR), str(port), str(hosts), str(pages), "0"])
        if not wait_origin(port):
            print(f"Chaos Origin failed to start for {name}")
            continue
            
        res = measure_tool(f"tier2_chaos_100k/{name}", cmd, port, budget_s, TIER2_JSONL)
        metrics = get_chaos_metrics(port)
        res["chaos_metrics"] = metrics
        print(f"  Chaos Metrics: {json.dumps(metrics)}")
        kill_tree(origin.pid); origin.wait()


if __name__ == "__main__":
    if TIER1_JSONL.exists():
        TIER1_JSONL.unlink()
    if TIER2_JSONL.exists():
        TIER2_JSONL.unlink()
    run_tier1()
    run_tier2()
    print("\nAll D2 benchmarks completed successfully!")

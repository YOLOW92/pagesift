#!/usr/bin/env python3
"""E1-D 每分钟采样。

CPU 用 /proc/stat 的全系统增量折算平均占用核数（进程集变化时按进程求差不可靠）；
RSS 取所有 pagesift 进程之和；输出/唯一 URL 统计所有 index.jsonl。
"""
import json, os, subprocess, sys, time
from pathlib import Path

out = Path(sys.argv[1]); t0 = int(sys.argv[2]); dur = int(sys.argv[3])
rec = out / "timeline.jsonl"
CLK = os.sysconf("SC_CLK_TCK")

def sys_cpu_ticks():
    with open("/proc/stat") as f:
        p = f.readline().split()[1:]
    # 只取"忙"的字段：user nice system irq softirq steal；排除 idle(3) 与 iowait(4)
    busy = [0, 1, 2, 5, 6, 7]
    return sum(int(p[i]) for i in busy)

def rss_mb():
    tot = 0
    r = subprocess.run(["pgrep", "-f", "pagesift"], capture_output=True, text=True)
    for pid in r.stdout.split():
        try:
            for line in Path(f"/proc/{pid}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    tot += int(line.split()[1]); break
        except Exception:
            pass
    return tot / 1024.0

def nprocs():
    r = subprocess.run(["pgrep", "-f", "pagesift"], capture_output=True, text=True)
    return len(r.stdout.split())

def count_outputs():
    urls, files = set(), 0
    for idx in out.glob("*/index.jsonl"):
        for line in idx.open(encoding="utf-8"):
            if not line.strip():
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            urls.add(r.get("url", "")); files += 1
    return files, len(urls)

def disk_mb():
    s = os.statvfs(str(out))
    return int((s.f_blocks - s.f_bfree) * s.f_frsize / 1e6)

prev_ticks = None; prev_t = None
while True:
    now = time.time()
    t = int(now - t0)
    if t > dur:
        break
    ticks = sys_cpu_ticks()
    if prev_ticks is None:
        cores = 0.0
    else:
        dt = max(now - prev_t, 0.001)
        cores = (ticks - prev_ticks) / CLK / dt
    prev_ticks, prev_t = ticks, now
    files, urls = count_outputs()
    rec.open("a", encoding="utf-8").write(json.dumps({
        "t_s": t, "procs": nprocs(), "cpu_cores": round(cores, 2),
        "rss_mb": round(rss_mb(), 1), "output_files": files,
        "unique_url": urls, "disk_used_mb": disk_mb(),
    }) + "\n")
    time.sleep(60)

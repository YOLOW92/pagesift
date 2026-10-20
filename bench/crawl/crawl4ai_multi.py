"""Crawl4AI 多进程：把站点平均分给 N 个 crawl4ai_crawl.py 进程（Crawl4AI 单进程受 Python 的 CPU 上限，
约 15 页/秒；部署时可以开多个进程横向扩展，这里按同样的方式给它机会）。

python crawl4ai_multi.py <python> <port> <hosts> <pages> <interval-ms> <http|browser> [per-host]
N = min(核数, 可用内存 / 2.5 GB（实测每个进程 2 GB 多）)，至少 1。
"""
import os, subprocess, sys

def mem_available_mb():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) // 1024
    return 0

python, port, hosts, pages, interval, mode = sys.argv[1], *sys.argv[2:7]
per_host = sys.argv[7] if len(sys.argv) > 7 else "2"
hosts = int(hosts)
n = max(1, min(os.cpu_count() or 1, (mem_available_mb() - 2048) // 2500, hosts))
here = os.path.dirname(os.path.abspath(__file__))
procs, start = [], 0
for i in range(n):
    count = hosts // n + (1 if i < hosts % n else 0)
    procs.append(subprocess.Popen([python, os.path.join(here, "crawl4ai_crawl.py"), port, str(count), pages,
                                   interval, mode, per_host, str(start)], stdout=subprocess.PIPE, text=True))
    start += count
ok = 0
for p in procs:
    out, _ = p.communicate()
    for line in out.splitlines():
        if line.startswith("crawl4ai-"):
            ok += int(line.split(": ", 1)[1].split()[0])
print(f"crawl4ai-{mode} x{n} processes: {ok} pages from {hosts} sites")

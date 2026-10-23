#!/usr/bin/env python3
"""Crawlee Multi-Process Runner: Eliminates single-thread GIL bottleneck.

Shards hosts evenly across N worker processes (default: min(32, cpu_count, hosts)).
Aggregates total pages and throughput.
"""
import os
import subprocess
import sys
import time

def main():
    if len(sys.argv) < 5:
        print("usage: crawlee_multi.py <port> <hosts> <pages> <interval-ms> [per-host]", file=sys.stderr)
        sys.exit(1)

    port, hosts, pages, interval = sys.argv[1:5]
    per_host = sys.argv[5] if len(sys.argv) > 5 else "2"
    total_hosts = int(hosts)

    num_workers = min(32, os.cpu_count() or 1, total_hosts)
    here = os.path.dirname(os.path.abspath(__file__))
    script = os.path.join(here, "crawlee_py_crawl.py")
    python_bin = sys.executable

    procs = []
    start = 0
    t0 = time.monotonic()
    for i in range(num_workers):
        count = total_hosts // num_workers + (1 if i < total_hosts % num_workers else 0)
        cmd = [python_bin, script, port, str(count), pages, interval, per_host, str(start)]
        procs.append(subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True))
        start += count

    total_pages = 0
    for p in procs:
        stdout, stderr = p.communicate()
        for line in (stdout + stderr).splitlines():
            if "crawlee-python: " in line:
                try:
                    total_pages += int(line.split("crawlee-python: ")[1].split()[0])
                except (ValueError, IndexError):
                    pass

    elapsed = time.monotonic() - t0
    throughput = total_pages / max(elapsed, 1e-6)
    print(f"crawlee-multi: {total_pages} pages from {total_hosts} sites across {num_workers} processes in {elapsed:.2f} s = {throughput:.0f} pages/s")

if __name__ == "__main__":
    main()

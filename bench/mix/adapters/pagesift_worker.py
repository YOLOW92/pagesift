"""pagesift 的 worker（协议见 README.md）：用发布的二进制（RULES 第三节），每份调一次，默认参数，不读配置文件。

    MIX_PAGESIFT_BIN=<二进制> python3 -I bench/mix/adapters/pagesift_worker.py <html|file|url>

- html：网页快照 → `pagesift convert <文件> [--base-url <原始 URL>]`（正文提取，默认只保留正文）
- file：PDF / Office / 电子书 → `pagesift scrape <文件>`
- url：抓取领域，回放服务上的地址 → `pagesift scrape --allow-cidr 127.0.0.1/32 <URL>`（只放行回环，其余默认）
每份一个子进程：单份耗时里包括进程启动（几毫秒）。测速轮另议。
"""
import json
import os
import subprocess
import sys
import time

BIN = os.environ["MIX_PAGESIFT_BIN"]
MODE = sys.argv[1]
ENV = {**os.environ, "PAGESIFT_CONFIG": ""}  # 空值 = 不读配置文件，全用默认


def version():
    out = subprocess.run([BIN, "--version"], capture_output=True, text=True, env=ENV).stdout.strip()
    return out or "pagesift ?"


def cmd(job):
    if MODE == "html":
        c = [BIN, "convert", job["input"]]
        if job.get("url"):
            c += ["--base-url", job["url"]]
        return c
    if MODE == "url":
        # 快照在本地回放服务（127.0.0.1）上；pagesift 默认拦截内网地址（SSRF 防护），用官方的 --allow-cidr 只放行回环这一个地址
        return [BIN, "scrape", "--allow-cidr", "127.0.0.1/32", job["input"]]
    return [BIN, "scrape", job["input"]]


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


send({"ready": True, "tool": f"pagesift-{MODE}", "version": version(), "pid": os.getpid()})
for line in sys.stdin:
    if not line.strip():
        continue
    job = json.loads(line)
    t = time.perf_counter()
    r = subprocess.run(cmd(job), capture_output=True, env=ENV)
    ms = (time.perf_counter() - t) * 1000
    if r.returncode != 0:
        send({"id": job["id"], "ok": False, "ms": ms, "bytes": 0, "error": r.stderr.decode("utf-8", "replace")[-500:] or f"exit {r.returncode}"})
        continue
    with open(job["out"], "wb") as f:
        f.write(r.stdout)
    send({"id": job["id"], "ok": True, "ms": ms, "bytes": len(r.stdout), "error": None})

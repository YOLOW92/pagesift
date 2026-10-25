#!/usr/bin/env python3
"""十域对决通用测量器：所有参赛者在同一个 cgroup 内存上限下跑同一批输入。

统一口径：
  - 容器 --memory / --memory-swap 均为上限值（默认 10 GiB），超限被 OOM kill 记失败
  - --cpuset-cpus 默认 0-15（本机全部逻辑核），不做人为限核
  - --network=host，避免 NAT 影响网络类项目
  - 峰值内存由 docker stats 轮询取 max，同时记录 OOMKilled
  - 墙钟由外部计时，吞吐 = 完成数 / 墙钟

用法:
  showdown_run.py --name trafilatura --image pagesift-bench:pytools \
      --input /data/d1/manifest.jsonl --out /out --workers 16 \
      --cmd "python /tools/d1_py.py --tool trafilatura --workers {workers}"
"""
import argparse, json, os, subprocess, sys, threading, time
from pathlib import Path

def parse_mem(s):
    return int(float(s.upper().replace("GIB", "").replace("MIB", "").replace("GB","").replace("MB","")) * (1024**3 if "G" in s.upper() else 1024**2))

def poll_stats(name, stop, samples):
    while not stop.is_set():
        r = subprocess.run(["docker", "stats", "--no-stream", "--format", "{{.MemUsage}}", name],
                           capture_output=True, text=True)
        if r.returncode == 0 and r.stdout.strip():
            used = r.stdout.strip().split("/")[0].strip()
            try:
                samples.append(parse_mem(used))
            except Exception:
                pass
        time.sleep(0.1)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", required=True)
    ap.add_argument("--image", required=True)
    ap.add_argument("--input", required=True, help="容器内输入路径")
    ap.add_argument("--out", required=True, help="容器内输出目录")
    ap.add_argument("--host-out", required=True, help="宿主机输出目录")
    ap.add_argument("--workers", type=int, default=16)
    ap.add_argument("--mem", default="10g")
    ap.add_argument("--cpuset", default="0-15")
    ap.add_argument("--cmd", required=True)
    ap.add_argument("--mounts", nargs="*", default=[], help="额外 -v 挂载")
    ap.add_argument("--result", required=True)
    a = ap.parse_args()

    root = Path.cwd()
    Path(a.host_out).mkdir(parents=True, exist_ok=True)
    cname = f"sd-{a.name}-{os.getpid()}"

    cmd = [ "docker","run","--name",cname,"--network=host",
            f"--memory={a.mem}", f"--memory-swap={a.mem}",
            f"--cpuset-cpus={a.cpuset}",
            "-v", f"{root}/bench/data:/data:ro",
            "-v", f"{root}/{a.host_out}:{a.out}",
            "-v", f"{root}/bench/scripts:/tools:ro" ]
    for m in a.mounts:
        cmd += ["-v", m]
    cmd += [a.image, "sh", "-lc", a.cmd.format(workers=a.workers, input=a.input, out=a.out)]

    stop = threading.Event(); samples = []
    t = threading.Thread(target=poll_stats, args=(cname, stop, samples), daemon=True); t.start()
    t0 = time.time()
    proc = subprocess.run(cmd, capture_output=True, text=True)
    wall = time.time() - t0
    stop.set(); t.join(timeout=2)

    insp = subprocess.run(["docker","inspect","--format","{{json .State}}", cname],
                          capture_output=True, text=True)
    state = json.loads(insp.stdout) if insp.returncode == 0 else {}
    subprocess.run(["docker","rm","-f",cname], capture_output=True)

    peak = max(samples) if samples else 0
    oom = bool(state.get("OOMKilled")) or proc.returncode == 137
    res = {
        "name": a.name, "image": a.image, "workers": a.workers,
        "mem_limit": a.mem, "wall_s": round(wall, 3),
        "exit_code": proc.returncode, "oom": oom,
        "peak_rss_bytes": peak, "peak_rss_source": "docker stats poll(100ms)",
        "cmd": a.cmd,
        "stderr_tail": proc.stderr[-1500:],
        "stdout_tail": proc.stdout[-1500:],
    }
    Path(a.result).parent.mkdir(parents=True, exist_ok=True)
    Path(a.result).write_text(json.dumps(res, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({k: res[k] for k in ("name","wall_s","exit_code","oom","peak_rss_bytes")}, ensure_ascii=False))

if __name__ == "__main__":
    main()

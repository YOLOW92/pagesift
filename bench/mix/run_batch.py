"""正式质量轮的批量调度：一次拿 bench.lock，同时跑多个“工具 × 子类”作业。

    python3 -I bench/mix/run_batch.py b1:docling b2:docling a2:crw a2:crawl4ai ...

- 锁：bench.lock 的 fd 设成可继承，传给每个 driver（MIX_LOCK_FD），driver 再传给它派生的 worker、Chromium、回放服务。
  flock 跟着打开的文件走，所以最后一个子进程退出之前锁一直被占着，不会出现启动器退了、子进程在锁外继续跑。
- 内存：这里不算。每个 driver 启动每一个 worker 之前都经过 admit.acquire：重新读 MemAvailable，减去全机所有还没
  涨到预计峰值的 worker 的预扣，保留 3 GB 给系统和桌面，活着的 worker 总数不超过核数。所以多个作业同时跑也不会超卖。
- 每个工具的 worker 在自己的 systemd slice 里，slice 有 MemoryMax 硬上限，超了只杀这个工具自己；单个 worker 另有
  limits.toml 的 4 GB 上限。
- 同时跑的 driver 最多 MIX_MAX_DRIVERS 个（默认 4）。收到 SIGTERM 就转发给各 driver，等它们把 worker 停干净再退出。
- 用模型的工具（docling*）只跑 samples/<子类>.tsv 里 model_subset = 1 的份；单份上限取 limits.toml。
"""
import fcntl
import os
import signal
import subprocess
import sys
import time
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
MIX = Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools"))
LOCK = os.environ.get("BENCH_LOCK", str(Path.home() / "pagesift/tasks/0.80/bench.lock"))
MAX_DRIVERS = int(os.environ.get("MIX_MAX_DRIVERS", "4"))
LIMITS = tomllib.loads((HERE / "limits.toml").read_text())
sys.path.insert(0, str(HERE))
import driver  # noqa: E402  只用它的 TOOLS 表


def manifest(sub, tool, limit=None):
    out = MIX / "runs/quality" / sub / "manifests"
    out.mkdir(parents=True, exist_ok=True)
    lines = (MIX / f"inputs/{sub}.manifest").read_text(encoding="utf-8").splitlines()
    if tool.startswith("docling"):
        rows = (HERE / f"samples/{sub}.tsv").read_text(encoding="utf-8").splitlines()
        cols = rows[0].split("\t")
        keep = {r["id"] for r in (dict(zip(cols, l.split("\t"))) for l in rows[1:]) if r.get("model_subset") == "1"}
        lines = [l for l in lines if l.split("\t", 1)[0] in keep]
    lines = lines[:limit] if limit else lines
    p = out / f"{tool}{'.first%d' % limit if limit else ''}.txt"
    p.write_text("".join(l + "\n" for l in lines), encoding="utf-8")
    return p, len(lines)


def main():
    jobs = []
    for spec in sys.argv[1:]:
        sub, tool, *limit = spec.split(":")  # 子类:工具[:N]，给了 N 只跑前 N 份（验证用）
        man, n = manifest(sub, tool, int(limit[0]) if limit else None)
        jobs.append({"sub": sub, "tool": tool, "man": man, "n": n, "mb": driver.TOOLS[tool]["per_worker_mb"]})
    jobs.sort(key=lambda j: -j["mb"])  # 吃内存大的先启动，免得后面被小作业的 worker 占满
    lockf = open(LOCK, "a")
    os.set_inheritable(lockf.fileno(), True)
    print(f"{time.strftime('%T')} 等 bench.lock", flush=True)
    fcntl.flock(lockf, fcntl.LOCK_EX)
    print(f"{time.strftime('%T')} 拿到 bench.lock，{len(jobs)} 个作业", flush=True)
    running = []
    stopping = []

    def on_term(signum, frame):
        stopping.append(1)
        for p, _ in running:
            p.send_signal(signal.SIGTERM)

    signal.signal(signal.SIGTERM, on_term)
    env = {**os.environ, "MIX_LOCK_FD": str(lockf.fileno())}
    while (jobs and not stopping) or running:
        for r in running[:]:
            if r[0].poll() is not None:
                running.remove(r)
                j = r[1]
                summ = (MIX / "runs/quality" / j["sub"] / f"{j['tool']}.summary.json").read_text()[:300]
                print(f"{time.strftime('%T')} 完成 {j['sub']}/{j['tool']}：{summ}", flush=True)
        if jobs and not stopping and len(running) < MAX_DRIVERS:
            j = jobs.pop(0)
            out = MIX / "runs/quality" / j["sub"]
            cmd = [sys.executable, "-I", str(HERE / "driver.py"), "--tool", j["tool"], "--manifest", str(j["man"]), "--out", str(out),
                   "--timeout", str(LIMITS["per_item_s"][j["sub"]]), "--mem-max", str(LIMITS["worker_mem_mb"])]
            p = subprocess.Popen(cmd, stdout=open(out / f"{j['tool']}.summary.json", "w"), stderr=open(out / f"{j['tool']}.driver.log", "w"),
                                 env=env, pass_fds=(lockf.fileno(),))
            running.append((p, j))
            print(f"{time.strftime('%T')} 启动 {j['sub']}/{j['tool']}：{j['n']} 份（worker 数由准入动态决定）", flush=True)
            continue
        time.sleep(1)
    print(f"{time.strftime('%T')} {'收到 SIGTERM，已停' if stopping else '全部完成'}，放锁", flush=True)


if __name__ == "__main__":
    main()

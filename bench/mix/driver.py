"""混合增强基准的统一调度：把一批输入交给某个工具的常驻 worker（协议见 adapters/README.md），并行跑，
每份记录状态、耗时、峰值内存。只用标准库：python3 -I bench/mix/driver.py ...

    python3 -I bench/mix/driver.py --tool trafilatura --manifest list.txt --out out/ [--workers auto|N]
           [--timeout 60] [--mem-max 4096] [--replay]

- manifest：每行一份输入，`id<TAB>路径[<TAB>原始 URL]` 或只写路径（id 取文件名）。原始 URL 随任务发给 worker（`url` 字段）。
- --replay：抓取类工具。driver 起一个本地快照回放服务（replay.py），把路径换成 http://127.0.0.1:<端口>/<相对路径>。
- 每个 worker 一个进程，用 systemd-run --user --scope 放进单独的 cgroup：MemoryMax 是单进程内存上限，
  峰值内存读 cgroup 的 memory.current（含 Chromium 等子进程）；超时就 cgroup.kill 整组杀掉，换一个新 worker 接着跑。
  没有 systemd 的机器退回普通子进程，内存按进程树 VmRSS 之和算，不设上限。
- 并发：--workers auto = min(核数, 份数, (MemAvailable − 保留) / 该工具每个 worker 的预计内存, 该工具上限)；
  给了 N 也会按内存压到能承受的数，并在汇总里写明实际用了几个。
- 输出：<out>/<tool>/<id>.md、results.jsonl（每份一行）、summary.json。
  状态：ok / empty（输出为空）/ error（工具报错）/ timeout / oom / crash（worker 退出）。
"""
import argparse
import json
import os
import queue
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import admit  # noqa: E402  跨 driver 的 worker 准入（内存、核数）
ADAPTERS = HERE / "adapters"
MIX = Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools"))
RUST = Path(os.environ.get("MIX_RUST_BIN", MIX / "rust/bin"))  # 编好后从 target/release 拷来的二进制和 extractous 的 libs/


def py(tool, venv=None, env=None, **kw):
    return dict(cmd=[str(MIX / "py" / (venv or tool) / "bin/python"), "-I", str(ADAPTERS / "py_worker.py"), tool], env=env or {}, **kw)


# per_worker_mb：每个 worker 的预计峰值内存（实测取整：Docling 2.5 GB，Crawl4AI 加 Chromium 0.7 GB），准入时按它预扣；
# max_workers：工具本身的限制（GPU 显存等）。
TOOLS = {
    # 网页正文（输入 HTML 文件）
    "crw": dict(cmd=[str(RUST / "mix-crw")], per_worker_mb=150),
    "trafilatura": py("trafilatura", per_worker_mb=200),
    "readability": dict(cmd=["node", str(ADAPTERS / "readability.mjs")], env={"MIX_NODE_DIR": str(MIX / "node/readability")}, per_worker_mb=400),
    # 抓取（输入 URL，配 --replay）
    "spider": dict(cmd=[str(RUST / "mix-spider")], per_worker_mb=150, url=True),
    "scrapy": py("scrapy", per_worker_mb=200, url=True),
    "crawl4ai": py("crawl4ai", env={"PLAYWRIGHT_BROWSERS_PATH": str(MIX / "ms-playwright"), "CRAWL4AI_NO_CLOUD_NOTICE": "1"}, per_worker_mb=700, url=True, startup_s=120),
    # PDF
    "pdf_oxide": dict(cmd=[str(RUST / "mix-pdf_oxide")], per_worker_mb=500),
    "pymupdf4llm": py("pymupdf4llm", per_worker_mb=500),
    # docling：默认安装（torch cu130，不支持 GTX 1070 的 sm_61）只能用 CPU；docling-gpu 用 PyTorch 官方 cu126 源的 torch
    "docling": py("docling", env={"DOCLING_DEVICE": "cpu", "HF_HOME": str(MIX / "hf")}, per_worker_mb=2500, startup_s=600),
    # docling-auto：默认安装、不指定设备（看 docling 在 GTX 1070 上自动选设备会怎样）
    "docling-auto": py("docling", env={"HF_HOME": str(MIX / "hf")}, per_worker_mb=2500, startup_s=600),
    "docling-gpu": py("docling", venv="docling-cu126", env={"DOCLING_DEVICE": "cuda", "HF_HOME": str(MIX / "hf")}, per_worker_mb=2500, max_workers=2, startup_s=600),
    # Office 与电子书
    "extractous": dict(cmd=[str(RUST / "mix-extractous")], env={"LD_LIBRARY_PATH": str(RUST / "libs")}, per_worker_mb=600),
    "extractous-py": py("extractous", per_worker_mb=600),
    "markitdown": py("markitdown", per_worker_mb=400),
}

CG_ROOT = Path("/sys/fs/cgroup")
LIVE = set()  # 活着的 Worker
LIVE_LOCK = threading.Lock()
STOP = threading.Event()
PEAK_LIVE = [0]
LOCK_FD = int(os.environ["MIX_LOCK_FD"]) if os.environ.get("MIX_LOCK_FD") else None  # 外层 bench.lock：传给子进程，最后一个退出才放锁
SLICE = {"name": None, "set": False}
HAVE_SYSTEMD = shutil.which("systemd-run") is not None and os.environ.get("MIX_NO_SYSTEMD") != "1"


def mem_available_mb():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) // 1024
    return 0


def tree_rss_mb(pid):
    """进程树 VmRSS 之和（没有 cgroup 时用）。"""
    total, todo, seen = 0, [pid], set()
    while todo:
        p = todo.pop()
        if p in seen:
            continue
        seen.add(p)
        try:
            with open(f"/proc/{p}/status") as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        total += int(line.split()[1])
            for t in os.listdir(f"/proc/{p}/task"):
                with open(f"/proc/{p}/task/{t}/children") as f:
                    todo += [int(c) for c in f.read().split()]
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            pass
    return total / 1024


class Worker:
    """一个常驻 worker 进程（及其 cgroup）。"""

    seq = 0
    seq_lock = threading.Lock()

    def __init__(self, name, spec, mem_max, log):
        self.name, self.spec, self.mem_max, self.log = name, spec, mem_max, log
        self.proc = None
        self.cg = None
        self.lines = queue.Queue()

    def start(self):
        if STOP.is_set():
            return None
        self.slot = admit.acquire(self.name, self.spec["per_worker_mb"])  # 每启动一个 worker 前都重新检查
        if STOP.is_set():
            self.slot.release()
            return None
        with Worker.seq_lock:
            Worker.seq += 1
            n = Worker.seq
        env = {**os.environ, **self.spec.get("env", {})}
        cmd = list(self.spec["cmd"])
        if HAVE_SYSTEMD:
            unit = f"mix-{self.name}-{os.getpid()}-{n}"
            cmd = ["systemd-run", "--user", "--scope", "--quiet", f"--unit={unit}", f"--slice={SLICE['name']}",
                   "-p", f"MemoryMax={self.mem_max}M", "-p", "MemorySwapMax=0", "--"] + cmd
        self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log,
                                     env=env, text=True, encoding="utf-8", start_new_session=True,
                                     pass_fds=(LOCK_FD,) if LOCK_FD is not None else ())
        with LIVE_LOCK:
            LIVE.add(self)
            PEAK_LIVE[0] = max(PEAK_LIVE[0], len(LIVE))
        self.lines = queue.Queue()
        threading.Thread(target=self._read, args=(self.proc, self.lines), daemon=True).start()
        t0 = time.perf_counter()
        line = self._get(time.perf_counter() + self.spec.get("startup_s", 60), None)
        if not isinstance(line, dict) or not line.get("ready"):
            self.kill()
            return None
        self.cg = self._cgroup()
        self.slot.set_cgroup(self.cg)
        set_slice_limit()
        self.oom_seen = self._oom_count()
        self.peak_fd = None
        if self.cg:
            # 内核 6.12 起往 memory.peak 写东西会把“这个 fd 看到的峰值”清零：每份之前清零、之后读，就是这一份的峰值
            try:
                self.peak_fd = os.open(self.cg / "memory.peak", os.O_RDWR)
            except OSError:
                self.peak_fd = None
        return {"startup_ms": (time.perf_counter() - t0) * 1000, "version": line.get("version")}

    @staticmethod
    def _read(proc, q):
        for line in proc.stdout:
            line = line.strip()
            if line.startswith("{"):
                try:
                    q.put(json.loads(line))
                except json.JSONDecodeError:
                    pass
        q.put(None)  # EOF：进程退出

    def _cgroup(self):
        if not HAVE_SYSTEMD:
            return None
        try:
            rel = Path(f"/proc/{self.proc.pid}/cgroup").read_text().strip().split("::", 1)[1]
            p = CG_ROOT / rel.lstrip("/")
            return p if (p / "memory.current").exists() else None
        except (OSError, IndexError):
            return None

    def _oom_count(self):
        if not self.cg:
            return 0
        try:
            for line in (self.cg / "memory.events").read_text().splitlines():
                if line.startswith("oom_kill "):
                    return int(line.split()[1])
        except OSError:
            pass
        return 0

    def mem_mb(self):
        if self.cg:
            try:
                return int((self.cg / "memory.current").read_text()) / 2**20
            except OSError:
                return 0.0
        return tree_rss_mb(self.proc.pid) if self.proc else 0.0

    def reset_peak(self):
        if self.peak_fd is not None:
            try:
                os.write(self.peak_fd, b"reset")
            except OSError:
                os.close(self.peak_fd)
                self.peak_fd = None

    def read_peak(self):
        if self.peak_fd is None:
            return 0.0
        try:
            return int(os.pread(self.peak_fd, 64, 0)) / 2**20
        except (OSError, ValueError):
            return 0.0

    def _get(self, deadline, peak):
        """等一行回复；peak 是 [当前峰值] 的可变列表，等的同时采样内存。超时返回 'timeout'。"""
        while True:
            left = deadline - time.perf_counter()
            if left <= 0:
                return "timeout"
            try:
                return self.lines.get(timeout=min(0.02, left))
            except queue.Empty:
                if peak is not None:
                    peak[0] = max(peak[0], self.mem_mb())

    def run(self, job, timeout):
        self.reset_peak()
        peak = [self.mem_mb()]
        t0 = time.perf_counter()
        try:
            self.proc.stdin.write(json.dumps(job, ensure_ascii=False) + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError):
            return {"status": "crash", "error": "worker stdin closed"}, peak[0]
        r = self._get(t0 + timeout, peak)
        wall = (time.perf_counter() - t0) * 1000
        peak[0] = max(peak[0], self.mem_mb(), self.read_peak())
        if r == "timeout":
            self.kill()
            return {"status": "timeout", "wall_ms": wall, "error": f"> {timeout} s"}, peak[0]
        if r is None:
            oom = self._oom_count() > self.oom_seen
            rc = self.proc.wait()
            self.kill()
            return {"status": "oom" if oom else "crash", "wall_ms": wall, "error": f"worker exited rc={rc}"}, peak[0]
        st = "ok" if r.get("ok") and r.get("bytes", 0) > 0 else ("empty" if r.get("ok") else "error")
        return {"status": st, "ms": r.get("ms"), "wall_ms": wall, "bytes": r.get("bytes", 0), "error": r.get("error")}, peak[0]

    def terminate(self, grace_s=20):
        """先给 cgroup 里每个进程发 SIGTERM，等它们自己退出；到点还没退才整组杀（Chromium 也在组里，不会遗留）。"""
        if self.cg:
            try:
                for pid in (self.cg / "cgroup.procs").read_text().split():
                    os.kill(int(pid), signal.SIGTERM)
            except (OSError, ValueError):
                pass
        elif self.proc:
            try:
                os.killpg(self.proc.pid, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                pass
        if self.proc:
            try:
                self.proc.wait(timeout=grace_s)
            except subprocess.TimeoutExpired:
                pass
        self.kill()

    def kill(self):
        if getattr(self, "slot", None) is not None:
            self.slot.release()
            self.slot = None
        with LIVE_LOCK:
            LIVE.discard(self)
        if getattr(self, "peak_fd", None) is not None:
            os.close(self.peak_fd)
            self.peak_fd = None
        if not self.proc:
            return
        if self.cg:
            try:
                (self.cg / "cgroup.kill").write_text("1")
            except OSError:
                pass
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        self.proc = None
        self.cg = None

    def close(self):
        if self.proc:
            try:
                self.proc.stdin.close()
                self.proc.wait(timeout=30)
            except (OSError, subprocess.TimeoutExpired):
                pass
            self.kill()


def set_slice_limit():
    """这个工具的全部 worker 合计的硬上限：内存总量减去保留量，超了只在这个工具的 slice 里杀进程。"""
    if not HAVE_SYSTEMD or SLICE["set"]:
        return
    SLICE["set"] = True
    with open("/proc/meminfo") as f:
        total = next(int(l.split()[1]) // 1024 for l in f if l.startswith("MemTotal:"))
    subprocess.run(["systemctl", "--user", "set-property", "--runtime", SLICE["name"],
                    f"MemoryMax={total - admit.RESERVE_MB}M", "MemorySwapMax=0"], check=False,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def on_sigterm(signum, frame):
    """收到 SIGTERM：不再领新份数，给活着的 worker 发 TERM 并等它们退出。"""
    if STOP.is_set():
        return
    STOP.set()

    def stop_all():
        with LIVE_LOCK:
            ws = list(LIVE)
        for w in ws:
            threading.Thread(target=w.terminate, daemon=True).start()
    threading.Thread(target=stop_all, daemon=True).start()


def read_manifest(path):
    items = []
    for i, line in enumerate(Path(path).read_text().splitlines()):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) >= 2:
            id_, inp = parts[0], parts[1]
        else:
            inp = line
            id_ = f"{i:05d}-{Path(inp).stem}"[:120]
        url = parts[2] if len(parts) >= 3 and parts[2] else None
        items.append((id_, inp, url))
    return items


def start_replay(items):
    """起本地快照回放服务，返回 (进程, 把路径换成 URL 的函数)。"""
    root = Path(os.path.commonpath([str(Path(p).resolve().parent) for _, p, _ in items]))
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    proc = subprocess.Popen([sys.executable, "-I", str(HERE / "replay.py"), str(root), str(port)],
                            pass_fds=(LOCK_FD,) if LOCK_FD is not None else ())
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.1).close()
            break
        except OSError:
            time.sleep(0.05)
    return proc, lambda p: f"http://127.0.0.1:{port}/" + str(Path(p).resolve().relative_to(root))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tool", required=True, choices=sorted(TOOLS))
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--workers", default="auto")
    ap.add_argument("--timeout", type=float, default=60.0, help="单份上限（秒），见 limits.toml")
    ap.add_argument("--mem-max", type=int, default=4096, help="单个 worker 的内存上限（MB）")
    ap.add_argument("--reserve", type=int, default=None, help="给系统保留的内存（MB，默认 3072，见 admit.py）")
    ap.add_argument("--replay", action="store_true", help="输入是快照文件，用本地回放服务转成 URL 给抓取工具")
    a = ap.parse_args()

    spec = TOOLS[a.tool]
    items = read_manifest(a.manifest)
    out = Path(a.out) / a.tool
    out.mkdir(parents=True, exist_ok=True)

    if a.reserve is not None:
        admit.RESERVE_MB = a.reserve
    # 线程数只是上限；每个线程启动 worker 前都要经过 admit.acquire（重新看 MemAvailable 和全机 worker 数），
    # 所以实际同时活着的 worker 数随内存动态变化，汇总里记 peak_live_workers
    cap = min(os.cpu_count() or 1, len(items) or 1, spec.get("max_workers", 1 << 30))
    n_workers = max(1, min(cap if a.workers == "auto" else int(a.workers), cap))
    SLICE["name"] = f"mix-{a.tool}-{os.getpid()}.slice"
    signal.signal(signal.SIGTERM, on_sigterm)

    replay, to_url = (None, None)
    if spec.get("url") or a.replay:
        replay, to_url = start_replay(items)

    jobs = queue.Queue()
    for it in items:
        jobs.put(it)
    res_lock = threading.Lock()
    res_f = open(out / "results.jsonl", "w", encoding="utf-8")
    log = open(out / "worker.log", "w")
    starts, peaks, versions = [], [], set()

    def loop():
        w = Worker(a.tool, spec, a.mem_max, log)
        alive, fails = False, 0
        while not STOP.is_set():
            if jobs.empty():
                break
            if not alive:
                # 先拿准入、起好 worker，再领份数：排队等准入的线程手里不压着份数
                info = w.start()
                if info is None:
                    if STOP.is_set():
                        break
                    fails += 1
                    try:
                        id_, inp, url = jobs.get_nowait()
                    except queue.Empty:
                        break
                    rec = {"status": "crash", "error": "worker failed to start"}
                    with res_lock:
                        res_f.write(json.dumps({"tool": a.tool, "id": id_, "input": inp, **rec}, ensure_ascii=False) + "\n")
                    if fails >= 3:
                        break
                    continue
                alive, fails = True, 0
                with res_lock:
                    starts.append(info["startup_ms"])
                    versions.add(info["version"])
            try:
                id_, inp, url = jobs.get_nowait()
            except queue.Empty:
                break
            job = {"id": id_, "input": to_url(inp) if to_url else str(Path(inp).resolve()), "out": str((out / f"{id_}.md").resolve())}
            if url and not to_url:
                job["url"] = url
            rec, peak = w.run(job, a.timeout)
            if STOP.is_set() and rec["status"] in ("crash", "timeout"):
                rec = {"status": "stopped", "error": "driver 收到 SIGTERM"}  # 被我们停掉的，不算工具失败
            alive = w.proc is not None
            with res_lock:
                peaks.append(peak)
                res_f.write(json.dumps({"tool": a.tool, "id": id_, "input": inp, **rec, "peak_mb": round(peak, 1)}, ensure_ascii=False) + "\n")
                res_f.flush()
        w.close()

    t0 = time.perf_counter()
    threads = [threading.Thread(target=loop) for _ in range(n_workers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - t0
    res_f.close()
    if replay:
        replay.terminate()

    # 一个 worker 连续起不来时它分到的份数就跑不到了，记成 crash，保证每份都有一行
    rows = [json.loads(l) for l in (out / "results.jsonl").read_text().splitlines()]
    done = {r["id"] for r in rows}
    with open(out / "results.jsonl", "a", encoding="utf-8") as f:
        for id_, inp, _ in items:
            if id_ not in done:
                r = {"tool": a.tool, "id": id_, "input": inp, "status": "stopped" if STOP.is_set() else "crash",
                     "error": "not run: driver stopped" if STOP.is_set() else "not run: workers failed"}
                rows.append(r)
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
    counts = {}
    for r in rows:
        counts[r["status"]] = counts.get(r["status"], 0) + 1
    summary = {
        "tool": a.tool, "versions": sorted(v for v in versions if v), "items": len(items),
        "workers": n_workers, "peak_live_workers": PEAK_LIVE[0], "workers_asked": a.workers, "wall_s": round(wall, 3),
        "reserve_mb": admit.RESERVE_MB, "per_worker_mb": spec["per_worker_mb"], "stopped": STOP.is_set(),
        "status": counts, "startup_ms": [round(s, 1) for s in starts],
        "max_peak_mb": round(max(peaks, default=0), 1), "timeout_s": a.timeout, "mem_max_mb": a.mem_max,
        "cgroup": HAVE_SYSTEMD,
    }
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=1))
    print(json.dumps(summary, ensure_ascii=False))


if __name__ == "__main__":
    main()

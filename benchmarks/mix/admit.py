"""跨进程的 worker 准入（内存与核数）：每启动一个 worker 进程之前都重新检查，同一台机器上所有 driver 共用一本账。

为什么要账本：Docling 这类工具启动后才慢慢把内存涨上去（模型在第一次转换时加载），只看启动那一刻的
MemAvailable 会超卖。所以每个活着的 worker 都登记“预计峰值”，还没涨到的部分（预计峰值 − cgroup 当前用量）
先预扣：
    可启动 ⇔ MemAvailable − Σ 预扣 − 新 worker 预计峰值 ≥ 保留量，且活着的 worker 数 < 核数
判断和登记在账本目录的 flock 下原子完成。账本在 $XDG_RUNTIME_DIR/mix-admit/，每个 worker 一个文件，
登记它的预计峰值、cgroup 路径和所属进程号；进程不在了的条目自动清掉。账本里一个 worker 都没有时总能启动一个，
免得因为别的程序占内存而永远等下去（单个 worker 还有 MemoryMax 硬上限兜底）。
"""
import fcntl
import json
import os
import time
from pathlib import Path

LEDGER = Path(os.environ.get("XDG_RUNTIME_DIR", "/tmp")) / "mix-admit"
RESERVE_MB = int(os.environ.get("MIX_RESERVE_MB", "3072"))  # 给系统和桌面保留
SLOTS = int(os.environ.get("MIX_SLOTS", os.cpu_count() or 1))  # 全机同时活着的 worker 上限


def mem_available_mb():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) // 1024
    return 0


def _current_mb(cg):
    if not cg:
        return 0.0
    try:
        return int((Path(cg) / "memory.current").read_text()) / 2**20
    except (OSError, ValueError):
        return 0.0


def _alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def _entries():
    out = []
    for p in LEDGER.glob("*.json"):
        try:
            e = json.loads(p.read_text())
        except (OSError, ValueError):
            continue
        if not _alive(e["owner"]):
            p.unlink(missing_ok=True)
            continue
        out.append((p, e))
    return out


class Slot:
    """一个已获准启动的 worker 的账本条目。"""

    def __init__(self, path, entry):
        self.path, self.entry = path, entry

    def set_cgroup(self, cg):
        self.entry["cg"] = str(cg) if cg else None
        self.path.write_text(json.dumps(self.entry))

    def release(self):
        self.path.unlink(missing_ok=True)


_seq = [0]


def acquire(tool, peak_mb, poll_s=1.0):
    """阻塞到可以启动一个预计峰值为 peak_mb 的 worker，返回 Slot。"""
    LEDGER.mkdir(parents=True, exist_ok=True)
    with open(LEDGER / ".lock", "a") as lf:
        while True:
            fcntl.flock(lf, fcntl.LOCK_EX)
            try:
                es = _entries()
                pending = sum(max(0.0, e["mb"] - _current_mb(e.get("cg"))) for _, e in es)
                room = mem_available_mb() - pending - peak_mb - RESERVE_MB
                if not es or (room >= 0 and len(es) < SLOTS):
                    _seq[0] += 1
                    entry = {"owner": os.getpid(), "tool": tool, "mb": peak_mb, "cg": None, "t": time.time()}
                    p = LEDGER / f"{os.getpid()}-{_seq[0]}.json"
                    p.write_text(json.dumps(entry))
                    return Slot(p, entry)
            finally:
                fcntl.flock(lf, fcntl.LOCK_UN)
            time.sleep(poll_s)

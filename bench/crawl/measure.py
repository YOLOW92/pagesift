"""运行一个命令，报告墙钟时间、CPU 时间和整个进程树（含浏览器等子进程）的峰值内存总和。

python3 -I measure.py <label> -- <command...>
结果追加一行 JSON 到 $MEASURE_OUT（默认 results.jsonl），同时打印。

保护机器：
- 内存看门狗：进程树的 PSS（按比例分摊共享内存后的真实占用）超过上限就整树杀掉，记为 "killed: memory"。上限按启动时的 MemAvailable
  减去 2 GB 余量动态算，也可以用 MEASURE_MAX_MB 指定。
- 时间预算：MEASURE_BUDGET_S 秒后整树杀掉，记为 "killed: budget"。
- 吞吐按源站实际送出的页数算（MEASURE_ORIGIN 指向源站的 /__served），对所有工具同一个口径，
  被杀掉的运行也能算出吞吐。
"""
import json, os, signal, subprocess, sys, time, resource, urllib.request

def pss_kb(pid):
    """按比例分摊共享页之后的占用：多进程程序（Chrome）各进程共享大量内存，RSS 直接相加会重复计算。"""
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return 0

def tree_pss_kb(root):
    return sum(pss_kb(p) for p in tree_pids(root))

def tree_rss_kb(root):
    # 遍历 /proc 找出 root 的所有后代，累加 RSS
    children = {}
    rss = {}
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

def mem_available_mb():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) // 1024
    return 0

def tree_pids(root):
    children = {}
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

def kill_tree(root):
    for pid in tree_pids(root):
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass

def served():
    url = os.environ.get("MEASURE_ORIGIN")
    if not url:
        return None
    try:
        with urllib.request.urlopen(url, timeout=5) as r:
            return int(r.read())
    except Exception:
        return None

def main():
    label = sys.argv[1]
    cmd = sys.argv[sys.argv.index("--") + 1:]
    limit_mb = min(12288, int(os.environ.get("MEASURE_MAX_MB") or min(12288, max(mem_available_mb() - 2048, 1024))))
    budget = float(os.environ.get("MEASURE_BUDGET_S") or 0)
    served0 = served()
    t = time.monotonic()
    scoped_cmd = [
        "systemd-run", "--user", "--scope", "-q",
        "-p", f"MemoryMax={limit_mb}M",
        "-p", "MemorySwapMax=0",
        "--", *cmd,
    ]
    proc = subprocess.Popen(scoped_cmd, stdout=subprocess.PIPE, text=True)
    peak = peak_pss = 0
    killed = None
    while proc.poll() is None:
        rss = tree_rss_kb(proc.pid)
        pss = tree_pss_kb(proc.pid)
        peak = max(peak, rss)
        peak_pss = max(peak_pss, pss)
        # 看门狗按 PSS：真实占用，不重复计算共享内存
        if pss // 1024 > limit_mb:
            killed = f"> 12 GB (killed by 12 GB memory limit: {pss // 1024} MB > {limit_mb} MB)"
        elif budget and time.monotonic() - t > budget:
            killed = f"budget {budget:.0f} s"
        if killed:
            kill_tree(proc.pid)
            proc.wait()
            break
        time.sleep(0.05)
    out = proc.stdout.read()
    if killed is None and proc.returncode in (-9, 137, -15, 143):
        killed = "> 12 GB (killed by 12 GB memory limit / cgroup OOM)"
    wall = time.monotonic() - t
    served1 = served()
    pages = served1 - served0 if served0 is not None and served1 is not None else None
    ru = resource.getrusage(resource.RUSAGE_CHILDREN)
    last = [l for l in out.splitlines() if l.strip()]
    rec = {"label": label, "wall_s": round(wall, 2), "cpu_s": round(ru.ru_utime + ru.ru_stime, 2),
           "peak_tree_rss_mb": round(peak / 1024, 1), "peak_tree_pss_mb": round(peak_pss / 1024, 1),
           "exit": proc.returncode,
           "served_pages": pages, "pages_per_s": round(pages / wall, 1) if pages is not None else None,
           "killed": killed, "summary": last[-1] if last else ""}
    print(json.dumps(rec, ensure_ascii=False))
    with open(os.environ.get("MEASURE_OUT", "results.jsonl"), "a") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")

main()

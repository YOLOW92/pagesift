"""运行一个命令，报告墙钟时间、CPU 时间和整个进程树（含浏览器等子进程）的峰值内存总和。

python3 -I measure.py <label> -- <command...>
结果追加一行 JSON 到 $MEASURE_OUT（默认 results.jsonl），同时打印。
"""
import json, os, subprocess, sys, time, resource

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

def main():
    label = sys.argv[1]
    cmd = sys.argv[sys.argv.index("--") + 1:]
    t = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True)
    peak = 0
    while proc.poll() is None:
        peak = max(peak, tree_rss_kb(proc.pid))
        time.sleep(0.05)
    out = proc.stdout.read()
    wall = time.monotonic() - t
    ru = resource.getrusage(resource.RUSAGE_CHILDREN)
    last = [l for l in out.splitlines() if l.strip()]
    rec = {"label": label, "wall_s": round(wall, 2), "cpu_s": round(ru.ru_utime + ru.ru_stime, 2),
           "peak_tree_rss_mb": round(peak / 1024, 1), "exit": proc.returncode,
           "summary": last[-1] if last else ""}
    print(json.dumps(rec, ensure_ascii=False))
    with open(os.environ.get("MEASURE_OUT", "results.jsonl"), "a") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")

main()

"""crw 整站爬取：启动 crw serve（单进程），通过 Firecrawl 兼容 API 同时提交每个站点的爬取任务，轮询到全部完成。

python3 -I crw_crawl.py <crw-binary> <port> <hosts> <pages> <interval-ms> [per-host]
礼貌规则对齐 pagesift：遵守 robots.txt，每主机最小间隔 interval，每主机并发 per-host。
crw 默认拦截本机地址，这里用它测试用的开关放行。打印 crw 进程树自身的峰值内存（不含本脚本）。
"""
import json, os, socket, subprocess, sys, threading, time, urllib.request

def rss_tree_kb(root):
    children, rss = {}, {}
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/stat") as f:
                ppid = int(f.read().rsplit(")", 1)[1].split()[1])
            with open(f"/proc/{pid}/statm") as f:
                rss[int(pid)] = int(f.read().split()[1]) * 4
            children.setdefault(ppid, []).append(int(pid))
        except (OSError, ValueError, IndexError):
            pass
    total, stack = 0, [root]
    while stack:
        p = stack.pop()
        total += rss.get(p, 0)
        stack.extend(children.get(p, []))
    return total

def call(method, url, body=None):
    req = urllib.request.Request(url, method=method, data=json.dumps(body).encode() if body else None,
                                 headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read())

def main():
    crw, port, hosts, pages, interval = sys.argv[1], *(int(x) for x in sys.argv[2:6])
    per_host = int(sys.argv[6]) if len(sys.argv) > 6 else 2
    api = 3917
    env = dict(os.environ,
               CRW_ALLOW_LOOPBACK_FOR_TESTS="1",
               CRW_SERVER__RATE_LIMIT_RPS="0",
               CRW_CRAWLER__RESPECT_ROBOTS_TXT="true",
               CRW_CRAWLER__PER_HOST_MIN_INTERVAL_MS=str(interval),
               CRW_CRAWLER__PER_HOST_MAX_CONCURRENT=str(per_host),
               CRW_CRAWLER__MAX_CONCURRENCY=str(max(hosts * per_host, 10)),
               CRW_CRAWLER__REQUESTS_PER_SECOND="100000")
    server = subprocess.Popen([crw, "serve", "--host", "127.0.0.1", "--port", str(api)], env=env,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", api), timeout=0.2).close()
            break
        except OSError:
            time.sleep(0.1)
    peak = [0]
    done = threading.Event()
    def sample():
        while not done.is_set():
            peak[0] = max(peak[0], rss_tree_kb(server.pid))
            time.sleep(0.05)
    threading.Thread(target=sample, daemon=True).start()
    t = time.monotonic()
    ids = [call("POST", f"http://127.0.0.1:{api}/v1/crawl", {
        "url": f"http://s{h}.localhost:{port}/", "limit": pages, "maxDepth": 100,
        "scrapeOptions": {"formats": ["markdown"]}})["id"] for h in range(hosts)]
    status = {}
    while True:
        for i in ids:
            if status.get(i, {}).get("status") not in ("completed", "failed", "cancelled"):
                r = call("GET", f"http://127.0.0.1:{api}/v1/crawl/{i}")
                status[i] = {"status": r.get("status"), "completed": r.get("completed", 0), "total": r.get("total", 0)}
        if all(s["status"] in ("completed", "failed", "cancelled") for s in status.values()):
            break
        time.sleep(0.05)
    s = time.monotonic() - t
    done.set()
    ok = sum(v["completed"] for v in status.values())
    server.terminate()
    server.wait()
    print(f"crw: {ok} pages from {hosts} sites in {s:.2f} s = {ok / s:.0f} pages/s, crw tree peak RSS {peak[0] / 1024:.1f} MB")

main()

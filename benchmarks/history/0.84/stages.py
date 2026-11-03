"""并发 1 逐页请求，收集响应里的分阶段耗时，输出每阶段中位数（毫秒）。stages.py <serve> <site> <页数>"""
import json, statistics, sys, time, urllib.request
API, SITE, N = sys.argv[1], sys.argv[2], int(sys.argv[3])
C = int(sys.argv[4]) if len(sys.argv) > 4 else 1
from concurrent.futures import ThreadPoolExecutor
def find(o):
    if isinstance(o, dict):
        if 'timings' in o and isinstance(o['timings'], dict): return o['timings']
        for v in o.values():
            r = find(v)
            if r is not None: return r
    return None
stages = {}; wall = []
def one(i):
    body = json.dumps({"url": f"{SITE}/p/{i}", "formats": ["markdown"], "engine": "http"}).encode()
    req = urllib.request.Request(f"{API}/v2/scrape", data=body, headers={"Content-Type": "application/json"})
    t = time.perf_counter()
    try: raw = urllib.request.urlopen(req, timeout=120).read()
    except urllib.error.HTTPError as e: raw = e.read()
    return (time.perf_counter() - t) * 1000, find(json.loads(raw)) or {}
t0 = time.perf_counter()
with ThreadPoolExecutor(C) as pool:
    res = list(pool.map(one, range(N)))
secs = time.perf_counter() - t0
for w, tm in res:
    wall.append(w)
    for k, v in tm.items():
        if isinstance(v, (int, float)): stages.setdefault(k, []).append(v)
out = {"pages_per_s": round(N / secs, 1), "client_wall": round(statistics.median(wall), 3)}
for k, v in sorted(stages.items()): out[k] = round(statistics.median(v), 3)
print(json.dumps(out))

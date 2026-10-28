#!/usr/bin/env python3
"""用 HTTP API 跑和 run.sh 相同的爬取，结果写成同样的目录结构（给命令行没有 --ignore-robots 的旧版本用）。
用法: api_run.py <服务地址> <输出目录> [主题...]
服务要用放行了忽略 robots 的配置启动：[policy] allow_crawl_ignore_robots = true"""
import concurrent.futures as cf, json, sys, time, urllib.request
from pathlib import Path

QUERIES = {
    "intl": "international students admission tuition scholarship visa",
    "grad": "graduate admission requirements PhD application deadline",
    "housing": "student housing accommodation residence",
    "cs": "computer science department faculty research",
    "library": "library opening hours borrowing",
}
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))  # 本机服务不走代理

def call(url, body=None):
    req = urllib.request.Request(url, data=json.dumps(body).encode() if body else None,
                                 headers={"content-type": "application/json"})
    with opener.open(req, timeout=60) as r:
        return json.load(r)

def crawl(base, out, topic, name, seed):
    d = out / topic; d.mkdir(parents=True, exist_ok=True)
    try:
        job = call(f"{base}/v2/crawl", {"url": seed, "limit": 12, "query": QUERIES[topic], "adaptive": True,
                                        "ignoreRobotsTxt": True, "scrapeOptions": {"formats": ["markdown"]}})
        deadline = time.time() + 420
        while True:
            st = call(f"{base}/v2/crawl/{job['id']}")
            if st["status"] in ("completed", "failed", "cancelled") or time.time() > deadline:
                break
            time.sleep(3)
    except Exception as e:  # 网络、服务错误：记下来，算这一站没结果
        (d / f"{name}.err").write_text(f"driver error: {e}\n")
        return f"{topic} {name} error"
    (d / name).mkdir(exist_ok=True)
    with open(d / name / "index.jsonl", "w") as f:
        for doc in st.get("data", []):
            m = doc.get("metadata", {})
            f.write(json.dumps({"url": m.get("url") or m.get("sourceURL"), "title": m.get("title"),
                                "status": m.get("statusCode")}) + "\n")
    p = st.get("pagesift") or {}
    (d / f"{name}.err").write_text(f"status: {st['status']}\nstopped: {p.get('stopReason')} (coverage {p.get('coverage')})\n")
    return f"{topic} {name} {st['status']} {len(st.get('data', []))}"

def main():
    base, out = sys.argv[1].rstrip("/"), Path(sys.argv[2])
    topics = sys.argv[3:] or list(QUERIES)
    sites = [l.split() for l in (Path(__file__).parent / "sites.txt").read_text().splitlines()
             if l.strip() and not l.startswith("#")]
    for topic in topics:
        with cf.ThreadPoolExecutor(8) as ex:
            for r in ex.map(lambda s: crawl(base, out, topic, s[0], s[1]), sites):
                print(r, flush=True)

if __name__ == "__main__":
    main()

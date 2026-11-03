#!/usr/bin/env python3
"""0.91 渲染决策 A/B：WebMainBench 545 页，`engine: auto` + `explain`，对一个正在运行的 serve 逐页请求。

  render_wmb.py <serve 地址> <站点地址> <WebMainBench_545.jsonl> <out.jsonl>

每页记下：是否渲染（plan 里 render 步 chosen）、是否采用渲染/合并结果、HTTP 的质量分 Q_http（render 步的
quality）、输出相对标准答案（groundtruth_content）的词级召回（小写 \\w+ 词的多重集交集 / 答案词数，
与 0.83 校准的召回定义相同）。站点按清单顺序 /p/<序号> 提供页面，全部页面同在一个路径族里。
"""
import collections
import json
import re
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

API, SITE, DATA, OUT = sys.argv[1:5]
truth = [json.loads(l).get("groundtruth_content") or "" for l in open(DATA, encoding="utf-8")]
WORD = re.compile(r"\w+")


def recall(out, ref):
    r = collections.Counter(WORD.findall(ref.lower()))
    if not r:
        return None
    o = collections.Counter(WORD.findall(out.lower()))
    return sum((r & o).values()) / sum(r.values())


def one(i):
    body = json.dumps({"url": f"{SITE}/p/{i}", "formats": ["markdown"], "engine": "auto",
                       "explain": True, "timeout": 60000}).encode()
    req = urllib.request.Request(f"{API}/v2/scrape", data=body, headers={"Content-Type": "application/json"})
    try:
        d = json.loads(urllib.request.urlopen(req, timeout=120).read()).get("data") or {}
    except Exception as e:  # noqa: BLE001 - 记下失败，继续
        return {"i": i, "error": str(e)}
    steps = (((d.get("metadata") or {}).get("pagesift") or {}).get("plan") or {}).get("steps") or []
    by = {s["action"]: s for s in steps}
    render = by.get("render", {})
    adopt = by.get("adopt") or by.get("merge") or {}
    return {"i": i, "rendered": bool(render.get("chosen")), "learned": render.get("learned"),
            "q_http": render.get("quality"), "adopted": bool(adopt.get("chosen")),
            "engine": ((d.get("metadata") or {}).get("pagesift") or {}).get("engine"),
            "recall": recall(d.get("markdown") or "", truth[i])}


with ThreadPoolExecutor(4) as pool, open(OUT, "w") as f:
    for r in pool.map(one, range(len(truth))):
        f.write(json.dumps(r) + "\n")

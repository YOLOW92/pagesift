#!/usr/bin/env python3
"""/v2/agent 评测：本地语料 + 本地搜索 + 带标准答案的问题集，不连实网（除了 --base-url 指向的模型服务）。

    # 录制回复（只验流程，结果不是质量数字）
    python3 -I benchmarks/history/1.0/agent/run.py

    # 真实模型（key 只从环境变量读，不写进任何文件）
    GROQ_API_KEY=... python3 -I benchmarks/history/1.0/agent/run.py \
        --base-url https://api.groq.com/openai/v1 --model openai/gpt-oss-120b \
        --api-key-env GROQ_API_KEY --effort low

先编译：cargo build --profile quick -p pagesift --bin pagesift --example agent_corpus
语料目录默认是主 checkout 的 benchmarks/data/snapshots，可用 --snapshots 或 PAGESIFT_AGENT_SNAPSHOTS 改。
结果写到 results/，每题写完就落盘，--resume 跳过已经跑完的题（免费档每天的 token 限额用完后，第二天接着跑）。
"""

import argparse
import http.server
import json
import os
import re
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
DEFAULT_SNAPSHOTS = "/home/yolow92/pagesift/benchmarks/data/snapshots"
# 录制模式假装的模型限额：和 Groq 免费档相同，好把预算和控速也走一遍
FAKE_CONTEXT, FAKE_COMPLETION, FAKE_TPM = 131072, 65536, 8000


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http_json(method, url, body=None, timeout=60):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method=method, headers={"content-type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def recorded_model(questions, base):
    """录制的模型：按提示里的题目找到这题的回复，按调用顺序给。返回 (服务, 地址)。"""
    scripts = {q["prompt"]: list(q["recorded"]) for q in questions}

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, body, headers=()):
            raw = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(raw)))
            for k, v in headers:
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            self.reply({"context_window": FAKE_CONTEXT, "max_completion_tokens": FAKE_COMPLETION})

        def do_POST(self):
            req = json.loads(self.rfile.read(int(self.headers["content-length"])))
            content = {}
            if req.get("max_tokens") != 1:
                user = req["messages"][-1]["content"]
                system = req["messages"][0]["content"]
                for prompt, script in scripts.items():
                    # extract 工具的 schema 生成提示里，题目在 intent 之外：按 intent 匹配
                    if f"Task:\n{prompt}\n" in user or (
                        "CSS extraction schemas" in system and script and "baseSelector" in script[0]
                    ):
                        content = script.pop(0) if script else {}
                        break
            text = json.dumps(content).replace("{base}", base)
            self.reply(
                {"choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": "stop"}]},
                [("x-ratelimit-limit-tokens", str(FAKE_TPM))],
            )

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_address[1]}/v1"


def normalize(s):
    return " " + re.sub(r"[\W_]+", " ", str(s).lower()).strip() + " "


def matches(accept, value):
    """按词比较；中文没有词边界，按子串比较。"""
    a, v = normalize(accept), normalize(value)
    return a in v or (a.strip() in v and any("\u4e00" <= c <= "\u9fff" for c in a))


def flat(v):
    if isinstance(v, list):
        return " ".join(flat(x) for x in v)
    return "" if v is None else str(v)


def score(q, status, base):
    """逐字段比标准答案；没有答案的题（expect: none）要求不编造、如实说缺失。"""
    data = status.get("data") if status.get("status") == "completed" else status.get("partial")
    data = data if isinstance(data, dict) else {}
    fields = q["fields"]
    ok = []
    for name, accept in fields.items():
        value = data.get(name)
        if accept is None:
            ok.append(value in (None, "", []))
        else:
            ok.append(any(matches(a, flat(value)) for a in accept))
    correct = all(ok)
    expect = q.get("expect", {})
    if expect.get("none"):
        correct = correct and (status.get("complete") is False)
    if expect.get("warning"):
        correct = correct and any(w.startswith(expect["warning"]) for w in status.get("warnings", []))
    if expect.get("stopReason"):
        correct = correct and status.get("stopReason") == expect["stopReason"]
    fs = status.get("fieldSources", {})
    gold = {f"{base}/p/{p}" for p in q.get("gold", [])}
    cited = {u for f in fs.values() for u in f["sources"]}
    return {
        "correct": correct,
        "fieldsCorrect": sum(ok),
        "fields": len(ok),
        "goldSourceFound": bool(gold) and gold <= cited,
        "fieldsVerified": sum(1 for f in fs.values() if f["verified"]),
        "fieldsWithSource": sum(1 for f in fs.values() if f["sources"]),
        "fieldsReported": len(fs),
    }


def summarize(rows):
    n = len(rows)
    pages = [r["pages"] for r in rows]
    tokens = [r["tokens"] for r in rows]
    gold = [r for r in rows if r["gold"]]
    return {
        "questions": n,
        "correct": sum(r["correct"] for r in rows),
        "accuracy": round(sum(r["correct"] for r in rows) / max(n, 1), 3),
        "pagesMean": round(statistics.fmean(pages), 2) if n else 0,
        "pagesMax": max(pages, default=0),
        "tokensMean": round(statistics.fmean(tokens), 1) if n else 0,
        "tokensTotal": sum(tokens),
        "tokensEstimated": any(r["tokensEstimated"] for r in rows),
        "llmCallsMean": round(statistics.fmean(r["llmCalls"] for r in rows), 2) if n else 0,
        "goldSourceFound": sum(r["goldSourceFound"] for r in gold),
        "questionsWithGold": len(gold),
        "fieldsVerified": sum(r["fieldsVerified"] for r in rows),
        "fieldsReported": sum(r["fieldsReported"] for r in rows),
        "searchRecallAt5": round(
            sum(r["searchHit"] for r in rows if r["searchHit"] is not None)
            / max(1, sum(1 for r in rows if r["searchHit"] is not None)),
            3,
        ),
        "failed": [r["id"] for r in rows if r.get("error")],
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--base-url", help="OpenAI-compatible service; without it the recorded replies are used")
    ap.add_argument("--model")
    ap.add_argument("--api-key-env", default="GROQ_API_KEY", help="name of the environment variable with the key")
    ap.add_argument("--llm-timeout-secs", type=int, help="[llm] timeout_secs, for slow model services")
    ap.add_argument("--effort", choices=["low", "medium", "high"], default="low")
    ap.add_argument("--snapshots", default=os.environ.get("PAGESIFT_AGENT_SNAPSHOTS", DEFAULT_SNAPSHOTS))
    ap.add_argument("--target-dir", default=os.environ.get("CARGO_TARGET_DIR", str(REPO / "target")))
    ap.add_argument("--profile", default="quick")
    ap.add_argument("--only", help="comma separated question ids")
    ap.add_argument("--limit", type=int, help="stop after this many questions")
    ap.add_argument("--resume", action="store_true")
    ap.add_argument("--out", help="results file (default results/recorded.json or results/live-<model>.json)")
    args = ap.parse_args()

    live = bool(args.base_url)
    if live and not args.model:
        ap.error("--base-url needs --model")
    if live and not os.environ.get(args.api_key_env):
        ap.error(f"environment variable {args.api_key_env} is not set")
    if not Path(args.snapshots).is_dir():
        ap.error(f"no snapshots at {args.snapshots} (they are not committed; use --snapshots)")

    questions = [json.loads(l) for l in (HERE / "questions.jsonl").read_text().splitlines() if l.strip()]
    if args.only:
        wanted = set(args.only.split(","))
        questions = [q for q in questions if q["id"] in wanted]
    name = "recorded" if not live else "live-" + re.sub(r"\W+", "-", args.model)
    out = Path(args.out) if args.out else HERE / "results" / f"{name}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    done = {}
    if args.resume and out.exists():
        done = {r["id"]: r for r in json.loads(out.read_text())["rows"] if not r.get("error")}

    bin_dir = Path(args.target_dir) / args.profile
    private = sorted({p for q in questions for p in q.get("private", [])})
    corpus_cmd = [str(bin_dir / "examples" / "agent_corpus"), "--snapshots", args.snapshots, "--port", "0"]
    if private:
        corpus_cmd += ["--private", ",".join(private)]
    corpus = subprocess.Popen(corpus_cmd, stdout=subprocess.PIPE, text=True)
    serve = fake = None
    rows = list(done.values())
    try:
        line = corpus.stdout.readline()
        if not line.startswith("ready"):
            sys.exit("the corpus server did not start")
        corpus_port = int(line.split()[1])
        base = f"http://127.0.0.1:{corpus_port}"
        if live:
            llm_url, model = args.base_url, args.model
        else:
            fake, llm_url = recorded_model(questions, base)
            model = "recorded"
        with tempfile.TemporaryDirectory() as tmp:
            config = Path(tmp) / "pagesift.toml"
            # 只写环境变量的名字，key 不进文件
            key_line = f'api_key = {{ env = "{args.api_key_env}" }}\n' if live else ""
            timeout_line = f"timeout_secs = {args.llm_timeout_secs}\n" if args.llm_timeout_secs else ""
            config.write_text(
                "[cache]\nenabled = false\n"
                '[fetch]\nallow_cidrs = ["127.0.0.0/8"]\n'
                f'[llm]\nbase_url = "{llm_url}"\nmodel = "{model}"\n{key_line}{timeout_line}'
                f'[search]\nbackend = "searxng"\nendpoint = "{base}"\n'
            )
            port = free_port()
            serve = subprocess.Popen(
                [str(bin_dir / "pagesift"), "--config", str(config), "serve", "--port", str(port),
                 "--data-dir", tmp, "--no-cache"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            api = f"http://127.0.0.1:{port}"
            for _ in range(200):
                try:
                    urllib.request.urlopen(f"{api}/healthz", timeout=1)
                    break
                except OSError:
                    time.sleep(0.1)
            else:
                sys.exit("pagesift serve did not start")

            for q in questions:
                if q["id"] in done:
                    continue
                if args.limit is not None and len(rows) - len(done) >= args.limit:
                    break
                body = {
                    "prompt": q["prompt"],
                    "effort": args.effort,
                    # 模型想打开别的网址时也出不去：全程不连实网
                    "allowedHosts": ["127.0.0.1"],
                }
                if q.get("schema"):
                    body["schema"] = q["schema"]
                if q.get("urls"):
                    body["urls"] = [u.replace("{base}", base) for u in q["urls"]]
                if q.get("maxCredits"):
                    body["maxCredits"] = q["maxCredits"]
                started = time.time()
                code, s = http_json("POST", f"{api}/v2/agent", body)
                if code != 200:
                    rows.append({"id": q["id"], "error": s, "correct": False, "gold": q.get("gold", []),
                                 "pages": 0, "tokens": 0, "llmCalls": 0, "tokensEstimated": False,
                                 "goldSourceFound": False, "fieldsVerified": 0, "fieldsReported": 0,
                                 "searchHit": None})
                    continue
                job = s["id"]
                while True:
                    _, status = http_json("GET", f"{api}/v2/agent/{job}")
                    if status.get("status") != "processing":
                        break
                    time.sleep(0.2 if not live else 2)
                search_hit = None
                if q.get("gold") and q.get("searchQueries"):
                    top = set()
                    for text in q["searchQueries"]:
                        _, found = http_json(
                            "GET", f"{base}/search?format=json&q=" + urllib.request.quote(text)
                        )
                        top |= {r["url"] for r in found["results"][:5]}
                    search_hit = all(f"{base}/p/{p}" in top for p in q["gold"])
                row = {
                    "id": q["id"],
                    "status": status.get("status"),
                    "finishReason": status.get("finishReason"),
                    "stopReason": status.get("stopReason"),
                    "pages": status.get("usage", {}).get("pages", 0),
                    "searches": status.get("usage", {}).get("searches", 0),
                    "llmCalls": status.get("usage", {}).get("llmCalls", 0),
                    "tokens": status.get("usage", {}).get("tokens", 0),
                    "tokensEstimated": status.get("usage", {}).get("tokensEstimated", False),
                    "seconds": round(time.time() - started, 1),
                    "answer": status.get("data") if status.get("status") == "completed" else status.get("partial"),
                    "warnings": status.get("warnings", []),
                    "error": status.get("error") if status.get("finishReason") == "error" else None,
                    "gold": q.get("gold", []),
                    "searchHit": search_hit,
                    "steps": status.get("steps", []),
                    **score(q, status, base),
                }
                rows.append(row)
                print(
                    f'{q["id"]}: {"ok" if row["correct"] else "WRONG"} status={row["status"]} '
                    f'finish={row["finishReason"]} pages={row["pages"]} tokens={row["tokens"]} '
                    f'calls={row["llmCalls"]} {row["seconds"]}s',
                    flush=True,
                )
                # 先写临时文件再替换：磁盘满时不丢已经花了 token 的结果
                partial = out.with_suffix(".tmp")
                partial.write_text(json.dumps(
                    {"mode": "live" if live else "recorded", "model": model, "effort": args.effort,
                     "summary": summarize(rows), "rows": rows},
                    ensure_ascii=False, indent=1))
                os.replace(partial, out)
    finally:
        for p in (serve, corpus):
            if p:
                p.terminate()  # SIGTERM：pagesift serve 要自己收尾
        for p in (serve, corpus):
            if p:
                p.wait(timeout=30)
        if fake:
            fake.shutdown()
    print(json.dumps(summarize(rows), ensure_ascii=False, indent=1))
    print(f"results: {out}")
    if not live:
        print("Recorded replies: this checks the flow, it says nothing about answer quality.")


if __name__ == "__main__":
    main()

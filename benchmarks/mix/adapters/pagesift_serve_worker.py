"""pagesift 的常驻 worker（协议见 README.md，第 2 版 RULES 第一节第 5 条的“官方推荐配置”）：
worker 启动时在本机起一个 `pagesift serve`（子进程，在同一个 cgroup 里，内存一起算），之后每份发一次本机 HTTP 请求。

    MIX_PAGESIFT_BIN=<二进制> python3 -I benchmarks/mix/adapters/pagesift_serve_worker.py <html|file|url>

- html：网页快照 → `POST /v2/parse`（上传文件，和网页相同的正文提取，默认只保留正文）
- file：PDF / Office / 电子书 → `POST /v2/parse`
- url：抓取领域，回放服务上的地址 → `POST /v2/scrape`（serve 加 `--allow-cidr 127.0.0.1/32`，只放行回环）
默认参数，不读配置文件，`--no-cache`（否则第二遍以后直接读缓存）。和每份一个进程的 pagesift_worker.py 相比，
单份耗时里没有进程启动。
"""
import http.client
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import uuid

BIN = os.environ["MIX_PAGESIFT_BIN"]
MODE = sys.argv[1]
ENV = {**os.environ, "PAGESIFT_CONFIG": ""}  # 空值 = 不读配置文件，全用默认


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start_server():
    port = free_port()
    data = tempfile.mkdtemp(prefix="mix-pagesift-serve-")
    cmd = [BIN, "serve", "--host", "127.0.0.1", "--port", str(port), "--no-cache", "--data-dir", data]
    if MODE == "url":
        cmd += ["--allow-cidr", "127.0.0.1/32"]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=sys.stderr, env=ENV)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return proc, port
        except OSError:
            if proc.poll() is not None:
                raise RuntimeError(f"pagesift serve exited with {proc.returncode}")
            time.sleep(0.05)
    raise RuntimeError("pagesift serve did not start within 60 s")


def multipart(path):
    boundary = uuid.uuid4().hex
    with open(path, "rb") as f:
        body = f.read()
    name = os.path.basename(path)
    head = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n"
            "Content-Type: application/octet-stream\r\n\r\n").encode()
    return head + body + f"\r\n--{boundary}--\r\n".encode(), f"multipart/form-data; boundary={boundary}"


def request(conn, job):
    if MODE == "url":
        body = json.dumps({"url": job["input"], "formats": ["markdown"]}).encode()
        ctype, path = "application/json", "/v2/scrape"
    else:
        body, ctype = multipart(job["input"])
        path = "/v2/parse"
    conn.request("POST", path, body=body, headers={"Content-Type": ctype})
    resp = conn.getresponse()
    data = resp.read()
    try:
        doc = json.loads(data)
    except ValueError:
        return None, f"HTTP {resp.status}: {data[:300]!r}"
    if not doc.get("success"):
        return None, f"HTTP {resp.status}: {json.dumps(doc.get('error') or doc, ensure_ascii=False)[:400]}"
    return (doc.get("data") or {}).get("markdown") or "", None


proc, port = start_server()
version = subprocess.run([BIN, "--version"], capture_output=True, text=True, env=ENV).stdout.strip() or "pagesift ?"
send({"ready": True, "tool": f"pagesift-serve-{MODE}", "version": version, "pid": os.getpid()})
conn = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
try:
    for line in sys.stdin:
        if not line.strip():
            continue
        job = json.loads(line)
        t = time.perf_counter()
        try:
            text, err = request(conn, job)
        except (http.client.HTTPException, OSError) as e:
            conn.close()
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
            text, err = None, f"{type(e).__name__}: {e}"
        ms = (time.perf_counter() - t) * 1000
        if err is not None:
            send({"id": job["id"], "ok": False, "ms": ms, "bytes": 0, "error": err})
            continue
        out = text.encode("utf-8")
        with open(job["out"], "wb") as f:
            f.write(out)
        send({"id": job["id"], "ok": True, "ms": ms, "bytes": len(out), "error": None})
finally:
    proc.terminate()  # SIGTERM：让 serve 正常退出（浏览器子进程一起收掉）
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()

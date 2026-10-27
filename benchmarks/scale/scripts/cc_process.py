#!/usr/bin/env python3
"""Common Crawl WARC 离线处理：抽 HTTP 200 HTML 响应，交 PageSift 转 Markdown。

流式读 gzip（WARC 记录按 Content-Length 定界），内存占用与段大小无关。
输出 manifest.jsonl（{track_id,url,html}）供 runner 使用，并统计各阶段计数。
"""
import gzip, json, sys
from pathlib import Path

def read_line(f):
    return f.readline()

def iter_records(path):
    """按 WARC 头 + Content-Length 流式切出每条记录。"""
    with gzip.open(path, "rb") as f:
        while True:
            line = f.readline()
            if not line:
                return
            if not line.startswith(b"WARC/"):
                continue
            head = {}
            while True:
                l = f.readline()
                if not l or l in (b"\r\n", b"\n"):
                    break
                k, _, v = l.partition(b":")
                head[k.strip().lower()] = v.strip()
            try:
                length = int(head.get(b"content-length", b"0"))
            except ValueError:
                length = 0
            body = f.read(length) if length else b""
            yield head, body

def parse_http(body: bytes):
    shead, _, payload = body.partition(b"\r\n\r\n")
    status = None
    ctype = ""
    for line in shead.decode("latin-1", "replace").split("\r\n"):
        low = line.lower()
        if low.startswith("http/"):
            parts = line.split()
            if len(parts) >= 2 and parts[1].isdigit():
                status = int(parts[1])
        elif low.startswith("content-type:"):
            ctype = line.split(":", 1)[1].strip().lower()
    return status, ctype, payload

def main():
    src = Path(sys.argv[1]); out = Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    stats = {"records": 0, "responses": 0, "html200": 0, "skipped_status": 0,
             "skipped_type": 0, "bytes_html": 0, "urls": set()}
    idx = 0
    with (out / "manifest.jsonl").open("w", encoding="utf-8") as man:
        for head, body in iter_records(src):
            stats["records"] += 1
            if head.get(b"warc-type") != b"response":
                continue
            stats["responses"] += 1
            status, ctype, payload = parse_http(body)
            if status != 200:
                stats["skipped_status"] += 1
                continue
            if "html" not in ctype:
                stats["skipped_type"] += 1
                continue
            html = payload.decode("utf-8", "replace")
            url = head.get(b"warc-target-uri", b"").decode("utf-8", "replace")
            man.write(json.dumps({"track_id": f"cc-{idx}", "url": url, "html": html},
                                 ensure_ascii=False) + "\n")
            stats["urls"].add(url)
            stats["bytes_html"] += len(payload)
            stats["html200"] += 1
            idx += 1
    stats["unique_url"] = len(stats.pop("urls"))
    (out / "warc_stats.json").write_text(json.dumps(stats, indent=2), encoding="utf-8")
    print(json.dumps(stats, indent=2))

if __name__ == "__main__":
    main()

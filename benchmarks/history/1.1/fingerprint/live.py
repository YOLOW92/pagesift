#!/usr/bin/env python3
"""实网核对：pagesift 开指纹模拟抓公开的回显站点和几个常见站点，每个只读一个页面。

    live.py <pagesift 二进制> [--chrome]

回显站点返回服务器看到的 JA4 和 HTTP/2 指纹。`--chrome` 同时用本机 Chrome 打开同一个回显页，方便对照。
"""
import json
import os
import re
import subprocess
import sys
import tempfile

ECHO = ["https://tls.peet.ws/api/all", "https://tls.browserleaks.com/json"]
SITES = ["https://www.cloudflare.com/", "https://www.google.com/", "https://www.example.com/"]


def pagesift(binary, url):
    env = dict(os.environ, PAGESIFT_CONFIG="")
    run = subprocess.run(
        [binary, "--browser-fingerprint", "chrome", "scrape", "--engine", "http", "--format", "json", url],
        capture_output=True, text=True, timeout=90, env=env,
    )
    text = run.stdout
    if "{" not in text:
        return {"error": (run.stdout + run.stderr)[:400]}
    doc = json.loads(text[text.index("{"):])
    if not doc.get("success"):
        return {"error": str(doc)[:400]}
    meta = doc["data"]["metadata"]
    return {
        "status": meta["statusCode"],
        "browserFingerprint": meta["pagesift"].get("browserFingerprint"),
        "warnings": meta["pagesift"].get("warnings"),
        "markdown": doc["data"].get("markdown", ""),
    }


def chrome(url):
    with tempfile.TemporaryDirectory() as profile:
        run = subprocess.run(
            ["google-chrome", "--headless=new", f"--user-data-dir={profile}", "--no-first-run", "--disable-gpu",
             "--disable-extensions", "--virtual-time-budget=4000", "--dump-dom", url],
            capture_output=True, text=True, timeout=60,
        )
    return run.stdout


def fields(text):
    """从回显页里挑出指纹字段。"""
    out = {}
    for key in ("ja4", "ja4_r", "akamai_fingerprint", "akamai_hash", "akamai_text", "ja3_hash"):
        match = re.search(rf'"{key}"\s*:\s*"([^"]*)"', text)
        if match:
            out[key] = match[1]
    return out


def main():
    binary = sys.argv[1]
    with_chrome = "--chrome" in sys.argv
    for url in ECHO:
        result = pagesift(binary, url)
        print(f"=== {url}")
        if "error" in result:
            print("pagesift error:", result["error"])
        else:
            print("pagesift", result["status"], result["browserFingerprint"], result["warnings"], fields(result["markdown"]))
        if with_chrome:
            print("chrome  ", fields(chrome(url)))
    for url in SITES:
        result = pagesift(binary, url)
        result.pop("markdown", None)
        print(f"=== {url}\n{result}")


if __name__ == "__main__":
    main()

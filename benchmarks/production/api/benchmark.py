#!/usr/bin/env python3
"""PageSift 0.40 Step 8 Baseline Benchmark:
Firecrawl v1/v2 API Protocol Compatibility & Jina Reader Architecture Baseline.
"""
import io, json, os, resource, shutil, subprocess, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit
import urllib.request
import urllib.error

ROOT = Path(__file__).resolve().parents[3]
PY = ROOT / "benchmarks/data/six/venv/bin/python"
BIN = ROOT / "target/release/pagesift"

def get_peak_rss_mb(pid):
    try:
        with open(f"/proc/{pid}/statm") as f:
            rss_pages = int(f.read().split()[1])
            return (rss_pages * 4096) / (1024 * 1024)
    except Exception:
        return 0.0

def make_http_request(url, method="GET", headers=None, data=None, timeout=10):
    if headers is None:
        headers = {}
    req = urllib.request.Request(url, method=method, headers=headers)
    if data is not None:
        if isinstance(data, (dict, list)):
            body = json.dumps(data).encode("utf-8")
            req.add_header("Content-Type", "application/json")
        elif isinstance(data, str):
            body = data.encode("utf-8")
        else:
            body = data
        req.data = body

    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            elapsed = time.perf_counter() - t0
            resp_body = resp.read()
            return {
                "status": resp.status,
                "headers": dict(resp.headers),
                "body": resp_body,
                "elapsed_ms": elapsed * 1000.0,
                "error": None,
            }
    except urllib.error.HTTPError as e:
        elapsed = time.perf_counter() - t0
        err_body = e.read()
        return {
            "status": e.code,
            "headers": dict(e.headers),
            "body": err_body,
            "elapsed_ms": elapsed * 1000.0,
            "error": str(e),
        }
    except Exception as e:
        elapsed = time.perf_counter() - t0
        return {
            "status": 0,
            "headers": {},
            "body": b"",
            "elapsed_ms": elapsed * 1000.0,
            "error": str(e),
        }

def run_benchmark():
    # 1. Start Mock Target Website
    class TargetSite(BaseHTTPRequestHandler):
        def do_GET(self):
            path = urlsplit(self.path).path
            if path == "/robots.txt":
                body = b"User-agent: *\nAllow: /\nSitemap: /sitemap.xml\n"
                mime = "text/plain"
            elif path == "/sitemap.xml":
                body = b"""<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>/article-1</loc></url>
  <url><loc>/article-2</loc></url>
</urlset>"""
                mime = "application/xml"
            elif path.startswith("/article-"):
                body = f"""<!DOCTYPE html>
<html>
<head><title>Article {path}</title></head>
<body>
  <article>
    <h1>High Performance Headline {path}</h1>
    <p>This is the main authoritative text body of article {path} containing detailed information for LLM indexing.</p>
  </article>
</body>
</html>""".encode("utf-8")
                mime = "text/html; charset=utf-8"
            else:
                self.send_response(404)
                self.end_headers()
                return

            self.send_response(200)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_): pass

    target_server = ThreadingHTTPServer(("127.0.0.1", 0), TargetSite)
    target_port = target_server.server_port
    target_base = f"http://127.0.0.1:{target_port}"
    th_target = threading.Thread(target=target_server.serve_forever, daemon=True)
    th_target.start()

    # 2. Start PageSift Server (0.30 baseline)
    with tempfile.TemporaryDirectory() as tmp_dir:
        tmp = Path(tmp_dir)
        cfg_file = tmp / "pagesift.toml"
        cfg_file.write_text("""[fetch]
allow_private_network = true

[policy]
interval_ms = 0

[server]
host = "127.0.0.1"
port = 0
""")
        server_port = 39821
        cmd_server = [
            str(BIN), "serve",
            "--config", str(cfg_file),
            "--port", str(server_port),
            "--host", "127.0.0.1",
            "--no-cache",
        ]

        t_start = time.perf_counter()
        srv_proc = subprocess.Popen(
            cmd_server,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )

        # Wait for server ready
        api_base = f"http://127.0.0.1:{server_port}"
        ready = False
        for _ in range(50):
            try:
                res = make_http_request(f"{api_base}/healthz", timeout=1)
                if res["status"] == 200:
                    ready = True
                    break
            except Exception:
                pass
            time.sleep(0.05)

        startup_ms = (time.perf_counter() - t_start) * 1000.0
        server_rss_mb = get_peak_rss_mb(srv_proc.pid)

        print(f"PageSift Server started: Ready={ready}, Startup={startup_ms:.1f}ms, RSS={server_rss_mb:.1f}MB")

        results = {
            "server": {
                "startup_ms": startup_ms,
                "peak_rss_mb": server_rss_mb,
                "binary_size_mb": BIN.stat().st_size / (1024 * 1024) if BIN.exists() else 0,
                "external_dependencies": "None (pure Safe Rust single binary)"
            },
            "firecrawl_v1": {},
            "firecrawl_v2": {},
            "jina_reader": {},
            "comparison": {
                "official_firecrawl_stack": {
                    "containers": ["api", "playwright", "postgres", "redis", "rabbitmq"],
                    "total_containers": 5,
                    "baseline_ram_mb": 2500.0,
                    "cold_start_s": 30.0,
                    "external_runtime": ["Docker", "Node.js", "Python", "PostgreSQL", "Redis", "RabbitMQ"]
                }
            }
        }

        if ready:
            # === [Test 1: Firecrawl v1 Scrape (/v1/scrape)] ===
            print("\nTesting Firecrawl v1 Scrape (/v1/scrape)...")
            scrape_url = f"{target_base}/article-1"
            v1_scrape_req = {
                "url": scrape_url,
                "formats": ["markdown", "html"],
                "onlyMainContent": True
            }
            res_v1_scrape = make_http_request(
                f"{api_base}/v1/scrape",
                method="POST",
                data=v1_scrape_req
            )
            print(f"  /v1/scrape: Status={res_v1_scrape['status']}, Latency={res_v1_scrape['elapsed_ms']:.1f}ms")
            results["firecrawl_v1"]["scrape"] = {
                "status": res_v1_scrape["status"],
                "latency_ms": res_v1_scrape["elapsed_ms"],
                "supported": res_v1_scrape["status"] == 200,
                "body_snippet": res_v1_scrape["body"][:200].decode("utf-8", errors="replace")
            }

            # === [Test 2: Firecrawl v1 Map (/v1/map)] ===
            print("\nTesting Firecrawl v1 Map (/v1/map)...")
            res_v1_map = make_http_request(
                f"{api_base}/v1/map",
                method="POST",
                data={"url": target_base}
            )
            print(f"  /v1/map: Status={res_v1_map['status']}, Latency={res_v1_map['elapsed_ms']:.1f}ms")
            results["firecrawl_v1"]["map"] = {
                "status": res_v1_map["status"],
                "latency_ms": res_v1_map["elapsed_ms"],
                "supported": res_v1_map["status"] == 200
            }

            # === [Test 3: Firecrawl v1 Crawl (/v1/crawl)] ===
            print("\nTesting Firecrawl v1 Crawl (/v1/crawl)...")
            res_v1_crawl = make_http_request(
                f"{api_base}/v1/crawl",
                method="POST",
                data={"url": target_base, "limit": 10}
            )
            print(f"  /v1/crawl: Status={res_v1_crawl['status']}, Latency={res_v1_crawl['elapsed_ms']:.1f}ms")
            results["firecrawl_v1"]["crawl"] = {
                "status": res_v1_crawl["status"],
                "latency_ms": res_v1_crawl["elapsed_ms"],
                "supported": res_v1_crawl["status"] == 200
            }

            # === [Test 4: Firecrawl v2 Scrape (/v2/scrape)] ===
            print("\nTesting Firecrawl v2 Scrape (/v2/scrape)...")
            res_v2_scrape = make_http_request(
                f"{api_base}/v2/scrape",
                method="POST",
                data=v1_scrape_req
            )
            print(f"  /v2/scrape: Status={res_v2_scrape['status']}, Latency={res_v2_scrape['elapsed_ms']:.1f}ms")
            results["firecrawl_v2"]["scrape"] = {
                "status": res_v2_scrape["status"],
                "latency_ms": res_v2_scrape["elapsed_ms"],
                "supported": res_v2_scrape["status"] == 200
            }

            # === [Test 5: Jina Reader (/r/https://...)] ===
            print("\nTesting Jina Reader (/r/https://...)...")
            res_jina = make_http_request(
                f"{api_base}/r/{target_base}/article-1",
                method="GET"
            )
            print(f"  GET /r/...: Status={res_jina['status']}, Latency={res_jina['elapsed_ms']:.1f}ms")
            results["jina_reader"]["direct_get"] = {
                "status": res_jina["status"],
                "latency_ms": res_jina["elapsed_ms"],
                "supported": res_jina["status"] == 200
            }

            # === [Test 6: Jina Reader SSE Streaming (/r/ with Accept: text/event-stream)] ===
            print("\nTesting Jina Reader SSE Streaming...")
            res_jina_sse = make_http_request(
                f"{api_base}/r/{target_base}/article-1",
                method="GET",
                headers={"Accept": "text/event-stream"}
            )
            print(f"  GET /r/ (SSE): Status={res_jina_sse['status']}, Latency={res_jina_sse['elapsed_ms']:.1f}ms")
            results["jina_reader"]["sse_streaming"] = {
                "status": res_jina_sse["status"],
                "supported": res_jina_sse["status"] == 200
            }

            # === [Test 7: Official Firecrawl Python SDK (V1FirecrawlApp & FirecrawlApp)] ===
            print("\nTesting Official Firecrawl Python SDK (V1FirecrawlApp & FirecrawlApp)...")
            sdk_test_script = f"""
import json
from firecrawl import V1FirecrawlApp, FirecrawlApp

v1_app = V1FirecrawlApp(api_url="{api_base}", api_key="dummy")
v2_app = FirecrawlApp(api_url="{api_base}", api_key="dummy")

v1_ok, v2_ok = False, False
v1_err, v2_err = "", ""

try:
    res1 = v1_app.scrape_url("{target_base}/article-1", formats=["markdown"])
    v1_ok = res1 is not None and getattr(res1, "markdown", None) is not None
except Exception as e:
    v1_err = str(e)

try:
    res2 = v2_app.scrape("{target_base}/article-1", formats=["markdown"])
    v2_ok = res2 is not None and (isinstance(res2, dict) or getattr(res2, "markdown", None) is not None)
except Exception as e:
    v2_err = str(e)

print("SDK_RESULT:", json.dumps({{"v1_ok": v1_ok, "v1_err": v1_err, "v2_ok": v2_ok, "v2_err": v2_err}}))
"""
            sdk_proc = subprocess.run(
                [str(PY), "-c", sdk_test_script],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True
            )
            v1_ok, v2_ok = False, False
            v1_err, v2_err = "", ""
            for line in sdk_proc.stdout.splitlines():
                if line.startswith("SDK_RESULT:"):
                    data = json.loads(line[len("SDK_RESULT:"):])
                    v1_ok = data.get("v1_ok", False)
                    v1_err = data.get("v1_err", "")
                    v2_ok = data.get("v2_ok", False)
                    v2_err = data.get("v2_err", "")
            print(f"  Official SDK Compatibility: V1_OK={v1_ok} (Err: {v1_err}), V2_OK={v2_ok} (Err: {v2_err})")
            results["firecrawl_v1"]["official_sdk_compatible"] = v1_ok
            results["firecrawl_v1"]["sdk_error"] = v1_err
            results["firecrawl_v2"]["official_sdk_compatible"] = v2_ok
            results["firecrawl_v2"]["sdk_error"] = v2_err

        # Stop server
        srv_proc.terminate()
        srv_proc.wait(timeout=5)
        target_server.shutdown()
        target_server.server_close()

    out_file = ROOT / "benchmarks/production/api/baseline_results.json"
    out_file.write_text(json.dumps(results, indent=2))
    print(f"\nBaseline benchmark written to {out_file}")

if __name__ == "__main__":
    run_benchmark()

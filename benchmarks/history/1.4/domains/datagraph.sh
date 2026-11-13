#!/usr/bin/env bash
# 1.4 DataGraph benchmark:
#   1. A/B latency & throughput on local real HTML pages (graph=off vs graph=on)
#   2. Multi-source StreamingGraphBuilder ingestion throughput & memory (1,000 docs)
#   3. End-to-end two-version local HTTP crawl with redirects, canonicals, JSON-LD,
#      attachments, and conflicting pricing + `pagesift graph inspect`
set -euo pipefail
cd "$(dirname "$0")/.."
source ./lib.sh
cd "$REPO"
check_bin
stage_begin datagraph

D=$RUN/datagraph
mkdir -p "$D"

log "datagraph: A/B HTML conversion (graph=off vs graph=on on 181 AEB HTML pages)"
locked taskset -c 4 "$EX/graph_bench" ab benchmarks/data/six/aeb > "$D/ab.json"

log "datagraph: StreamingGraphBuilder 1,000-doc cross-source ingestion"
locked taskset -c 4 "$EX/graph_bench" stream 1000 > "$D/stream.json"

log "datagraph: E2E local two-version crawl + graph inspect"
"$PY" -I - "$BIN" "$D" <<'PY' > "$D/e2e.json"
import json, subprocess, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

bin_path, out_dir = sys.argv[1], Path(sys.argv[2])
phase = ["v1"]

class Site(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/robots.txt":
            self._send(200, b"User-agent: *\nAllow: /\n", "text/plain")
        elif self.path == "/old-product":
            self.send_response(301)
            self.send_header("Location", "/product")
            self.end_headers()
        elif self.path in ("/", "/index.html"):
            html = (
                "<!doctype html><html><head><title>Catalog</title></head><body>"
                "<main><h1>Catalog</h1>"
                "<a href='/old-product'>Product Alpha</a> "
                "<a href='/product-alias'>Product Alias</a> "
                "<a href='/partner-note'>Partner Note</a> "
                "<a href='/spec.txt'>Spec Attachment</a></main></body></html>"
            ).encode()
            self._send(200, html, "text/html; charset=utf-8")
        elif self.path == "/product-alias":
            body = """<!doctype html><html><head>
<title>Product Alpha Alias</title>
<link rel="canonical" href="/product">
</head><body><main><h1>Product Alpha Alias</h1>
<p>Canonical page at <a href="/product">Product Alpha</a>.</p>
</main></body></html>""".encode()
            self._send(200, body, "text/html; charset=utf-8")
        elif self.path == "/product":
            price = "$99.00" if phase[0] == "v1" else "$129.00"
            body = f"""<!doctype html><html><head>
<title>Product Alpha</title>
<link rel="canonical" href="/product">
<script type="application/ld+json">{{"@type":"Product","sku":"SKU-ALPHA","price":"{price}","releaseDate":"2026-11-01"}}</script>
</head><body><main>
<h1>Product Alpha</h1>
<p class="price">Current list price: {price} per seat for SKU-ALPHA.</p>
<p>Download the <a href="/spec.txt">datasheet</a> or review <a href="/partner-note">partner pricing</a>.</p>
</main></body></html>""".encode()
            self._send(200, body, "text/html; charset=utf-8", etag=f'"prod-{phase[0]}"')
        elif self.path == "/partner-note":
            price = "$99.00" if phase[0] == "v1" else "$149.00"
            body = f"""<!doctype html><html><head>
<title>Partner Note</title>
<script type="application/ld+json">{{"@type":"Offer","sku":"SKU-ALPHA","price":"{price}","releaseDate":"2026-11-10"}}</script>
</head><body><main>
<h1>Partner Distribution Note</h1>
<p>Reseller tier for SKU-ALPHA lists at {price} per seat.</p>
</main></body></html>""".encode()
            self._send(200, body, "text/html; charset=utf-8", etag=f'"partner-{phase[0]}"')
        elif self.path == "/spec.txt":
            self._send(200, b"SKU-ALPHA technical datasheet v1.\n", "text/plain")
        else:
            self._send(404, b"not found", "text/plain")

    def _send(self, code, data, ctype, etag=None):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        if etag:
            self.send_header("ETag", etag)
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass

srv = ThreadingHTTPServer(("127.0.0.1", 0), Site)
threading.Thread(target=srv.serve_forever, daemon=True).start()
base = f"http://127.0.0.1:{srv.server_port}"

def run_crawl(tag, ver):
    phase[0] = ver
    cdir = out_dir / f"crawl_{tag}"
    t0 = time.perf_counter()
    subprocess.run([
        bin_path, "crawl", f"{base}/",
        "--allow-cidr", "127.0.0.0/8",
        "--no-politeness", "--no-robots",
        "--graph", "--subject-keys", "sku",
        "--out", str(cdir),
    ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    crawl_ms = (time.perf_counter() - t0) * 1000.0
    insp = subprocess.check_output([bin_path, "graph", "inspect", str(cdir), "--json"], text=True)
    return crawl_ms, json.loads(insp)

ms1, g1 = run_crawl("v1", "v1")
ms2, g2 = run_crawl("v2", "v2")
# Combine v1 + v2 jsonl deltas through graph inspect by concatenating into combined_deltas.jsonl
comb = out_dir / "combined_deltas.jsonl"
comb.write_text(
    (out_dir / "crawl_v1" / "graph.jsonl").read_text() +
    (out_dir / "crawl_v2" / "graph.jsonl").read_text()
)
insp_comb = json.loads(subprocess.check_output([bin_path, "graph", "inspect", str(comb), "--json"], text=True))
srv.shutdown()
srv.server_close()

print(json.dumps({
    "v1_ms": round(ms1, 2),
    "v2_ms": round(ms2, 2),
    "v1_stats": g1["stats"],
    "v2_stats": g2["stats"],
    "combined_stats": insp_comb["stats"],
}, indent=2))
PY

"$PY" -I - "$D/ab.json" "$D/stream.json" "$D/e2e.json" <<'PY' >> "$RUN/metrics.tsv"
import json, sys
from pathlib import Path

ab = json.loads(Path(sys.argv[1]).read_text())
print(f"graph.ab.pages\t{ab['pages']}")
print(f"graph.ab.off_p50_ms\t{ab['off']['p50_ms']:.6f}")
print(f"graph.ab.on_p50_ms\t{ab['on']['p50_ms']:.6f}")
print(f"graph.ab.off_p95_ms\t{ab['off']['p95_ms']:.6f}")
print(f"graph.ab.on_p95_ms\t{ab['on']['p95_ms']:.6f}")
print(f"graph.ab.off_pages_s\t{ab['off']['pages_s']:.3f}")
print(f"graph.ab.on_pages_s\t{ab['on']['pages_s']:.3f}")
print(f"graph.ab.p50_overhead_pct\t{ab['p50_overhead_pct']:.2f}")
print(f"graph.ab.off_rss_mb\t{ab['off']['vm_hwm_mb']:.2f}")
print(f"graph.ab.on_rss_mb\t{ab['on']['vm_hwm_mb']:.2f}")
print(f"graph.ab.blocks\t{ab['on']['blocks']}")
print(f"graph.ab.citations\t{ab['on']['citations']}")
print(f"graph.ab.provenance\t{ab['on']['provenance']}")
print(f"graph.ab.evidence\t{ab['on']['evidence']}")
print(f"graph.ab.claims\t{ab['on']['claims']}")

st = json.loads(Path(sys.argv[2]).read_text())
print(f"graph.stream.docs\t{st['docs']}")
print(f"graph.stream.docs_s\t{st['docs_s']:.2f}")
print(f"graph.stream.ingest_ms\t{st['ingest_ms']:.3f}")
print(f"graph.stream.finalize_ms\t{st['finalize_ms']:.3f}")
print(f"graph.stream.rss_mb\t{st['vm_hwm_mb']:.2f}")
for k, v in st["stats"].items():
    print(f"graph.stream.{k}\t{v}")

e2e = json.loads(Path(sys.argv[3]).read_text())
for k, v in e2e["combined_stats"].items():
    print(f"graph.e2e.{k}\t{v}")
print(f"graph.e2e.v1_ms\t{e2e['v1_ms']}")
print(f"graph.e2e.v2_ms\t{e2e['v2_ms']}")
PY

stage_end datagraph

#!/usr/bin/env python3
"""PageSift 0.40 Step 7 Baseline Benchmark:
Stream Ingestion (yt-dlp vs FFmpeg vs PageSift) & WARC Archiving (warcio vs PageSift).
"""
import io, json, os, resource, shutil, subprocess, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
PY = ROOT / "benchmarks/data/six/venv/bin/python"
BIN = ROOT / "target/release/pagesift"

def get_peak_rss_mb():
    # ru_maxrss is in KiB on Linux
    return resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024.0

def generate_fixtures(streams_dir):
    streams_dir.mkdir(parents=True, exist_ok=True)
    hls_dir = streams_dir / "hls"
    dash_dir = streams_dir / "dash"
    hls_dir.mkdir(exist_ok=True)
    dash_dir.mkdir(exist_ok=True)

    # Generate 5-second multi-segment streams with 1s segments (5 segments each)
    common = [
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100",
        "-t", "5",
        "-c:v", "libx264", "-preset", "ultrafast", "-threads", "2", "-g", "25",
        "-c:a", "aac", "-b:a", "64k"
    ]

    subprocess.run(common + [
        "-f", "hls", "-hls_time", "1", "-hls_list_size", "0",
        "-hls_playlist_type", "vod", str(hls_dir / "master.m3u8")
    ], check=True)

    subprocess.run(common + [
        "-f", "dash", "-seg_duration", "1",
        "-use_template", "1", "-use_timeline", "1",
        str(dash_dir / "manifest.mpd")
    ], check=True)

def run_cmd_measured(cmd, cwd=None):
    start_rss = get_peak_rss_mb()
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = time.perf_counter() - t0
    peak_rss = get_peak_rss_mb()
    return {
        "exit_code": p.returncode,
        "elapsed_s": elapsed,
        "peak_rss_mb": peak_rss,
        "stdout": p.stdout.decode("utf-8", errors="replace"),
        "stderr": p.stderr.decode("utf-8", errors="replace")
    }

def probe_stream(path):
    cmd = [
        "ffprobe", "-v", "error", "-count_frames",
        "-show_entries", "stream=codec_type,codec_name,nb_read_frames",
        "-of", "json", str(path)
    ]
    p = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        data = json.loads(p.stdout)
        frames = {s["codec_type"]: int(s.get("nb_read_frames", 0)) for s in data.get("streams", [])}
        return frames
    except Exception:
        return {}

def benchmark_streams(base_url, streams_dir):
    print("=== [1] Stream Ingestion Baseline Benchmark (HLS & MPEG-DASH) ===")
    results = {}

    hls_url = f"{base_url}/streams/hls/master.m3u8"
    dash_url = f"{base_url}/streams/dash/manifest.mpd"

    # Reference frame count
    ref_frames = probe_stream(streams_dir / "hls" / "master.m3u8")
    print(f"Reference Stream Frames: {ref_frames}")

    with tempfile.TemporaryDirectory() as tmp_dir:
        tmp = Path(tmp_dir)

        # 1. yt-dlp
        print("\nBenchmarking yt-dlp 2026.10.21...")
        yt_out = tmp / "yt_hls.mp4"
        cmd_yt = [
            str(PY), "-m", "yt_dlp", "--no-warnings", "-q",
            "-o", str(yt_out), hls_url
        ]
        res_yt = run_cmd_measured(cmd_yt)
        frames_yt = probe_stream(yt_out) if yt_out.exists() else {}
        results["yt_dlp_hls"] = {
            "elapsed_s": res_yt["elapsed_s"],
            "peak_rss_mb": res_yt["peak_rss_mb"],
            "frames": frames_yt,
            "intact": frames_yt == ref_frames,
            "external_deps": ["python", "ffmpeg"]
        }
        print(f"  yt-dlp HLS: {res_yt['elapsed_s']:.3f}s, Peak RSS: {res_yt['peak_rss_mb']:.1f} MB, Intact: {frames_yt == ref_frames}")

        # 2. FFmpeg directly
        print("\nBenchmarking FFmpeg 6.1.1 (C Native)...")
        ff_out = tmp / "ff_hls.ts"
        cmd_ff = [
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-i", hls_url, "-c", "copy", str(ff_out)
        ]
        res_ff = run_cmd_measured(cmd_ff)
        frames_ff = probe_stream(ff_out) if ff_out.exists() else {}
        results["ffmpeg_hls"] = {
            "elapsed_s": res_ff["elapsed_s"],
            "peak_rss_mb": res_ff["peak_rss_mb"],
            "frames": frames_ff,
            "intact": frames_ff == ref_frames,
            "external_deps": ["ffmpeg"]
        }
        print(f"  FFmpeg HLS: {res_ff['elapsed_s']:.3f}s, Peak RSS: {res_ff['peak_rss_mb']:.1f} MB, Intact: {frames_ff == ref_frames}")

        # 3. PageSift stream-download (0.30 baseline)
        if BIN.exists():
            print("\nBenchmarking PageSift (0.30 Stream Baseline)...")
            cfg = tmp / "pagesift.toml"
            cfg.write_text("[fetch]\nallow_private_network=true\n[policy]\ninterval_ms=0\n[cache]\nenabled=false\n")
            ps_out = tmp / "ps_hls"
            cmd_ps = [
                str(BIN), "--config", str(cfg), "stream-download", hls_url, "--out", str(ps_out)
            ]
            res_ps = run_cmd_measured(cmd_ps)
            ps_files = list(ps_out.rglob("*.ts"))
            frames_ps = probe_stream(ps_files[0]) if ps_files else {}
            results["pagesift_hls"] = {
                "elapsed_s": res_ps["elapsed_s"],
                "peak_rss_mb": res_ps["peak_rss_mb"],
                "frames": frames_ps,
                "intact": frames_ps == ref_frames,
                "external_deps": ["none (pure Rust)"]
            }
            print(f"  PageSift HLS: {res_ps['elapsed_s']:.3f}s, Peak RSS: {res_ps['peak_rss_mb']:.1f} MB, Intact: {frames_ps == ref_frames}")

        # DASH Benchmarking
        print("\n--- DASH Benchmarking ---")
        ref_dash_frames = probe_stream(streams_dir / "dash" / "manifest.mpd")
        print(f"Reference DASH Frames: {ref_dash_frames}")

        # yt-dlp DASH
        yt_dash_out = tmp / "yt_dash.mp4"
        cmd_yt_dash = [
            str(PY), "-m", "yt_dlp", "--no-warnings", "-q",
            "-o", str(yt_dash_out), dash_url
        ]
        res_yt_dash = run_cmd_measured(cmd_yt_dash)
        frames_yt_dash = probe_stream(yt_dash_out) if yt_dash_out.exists() else {}
        results["yt_dlp_dash"] = {
            "elapsed_s": res_yt_dash["elapsed_s"],
            "peak_rss_mb": res_yt_dash["peak_rss_mb"],
            "frames": frames_yt_dash,
            "intact": frames_yt_dash == ref_dash_frames,
            "external_deps": ["python", "ffmpeg"]
        }
        print(f"  yt-dlp DASH: {res_yt_dash['elapsed_s']:.3f}s, Peak RSS: {res_yt_dash['peak_rss_mb']:.1f} MB, Intact: {frames_yt_dash == ref_dash_frames}")

        # PageSift DASH
        if BIN.exists():
            ps_dash_out = tmp / "ps_dash"
            cmd_ps_dash = [
                str(BIN), "--config", str(cfg), "stream-download", dash_url, "--out", str(ps_dash_out)
            ]
            res_ps_dash = run_cmd_measured(cmd_ps_dash)
            results["pagesift_dash"] = {
                "elapsed_s": res_ps_dash["elapsed_s"],
                "peak_rss_mb": res_ps_dash["peak_rss_mb"],
                "exit_code": res_ps_dash["exit_code"],
                "external_deps": ["none (pure Rust)"]
            }
            print(f"  PageSift DASH: {res_ps_dash['elapsed_s']:.3f}s, Peak RSS: {res_ps_dash['peak_rss_mb']:.1f} MB, Exit: {res_ps_dash['exit_code']}")

    return results

def benchmark_warc():
    print("\n=== [2] ISO 28500 WARC/1.1 Archiving Baseline (warcio 1.8.1) ===")
    import warcio
    from warcio.warcwriter import WARCWriter
    from warcio.statusandheaders import StatusAndHeaders

    num_records = 1000
    # Create sample assets of various sizes (HTML, CSS, JS, PNG bytes)
    sample_payloads = [
        (b"<!DOCTYPE html><html><head><title>Test Page</title></head><body><h1>Hello World</h1><p>" + b"Paragraph data content " * 100 + b"</p></body></html>", "text/html"),
        (b"body { font-family: sans-serif; margin: 0; padding: 20px; }\n.header { background: #333; color: white; }" * 50, "text/css"),
        (b"function init() { console.log('Initializing application context'); let x = 0; for(let i=0; i<100; i++) x += i; }" * 60, "application/javascript"),
        (b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15c4\x00\x00\x00\nIDATx\x9cc\x00\x01\x00\x00\x05\x00\x01\r\n-\xb4\x00\x00\x00\x00IEND\xaeB`\x82" * 40, "image/png"),
    ]

    total_bytes = 0
    records_data = []
    for i in range(num_records):
        payload, mime = sample_payloads[i % len(sample_payloads)]
        url = f"https://example.com/page_{i}"
        records_data.append((url, payload, mime))
        total_bytes += len(payload)

    print(f"Total Transactions: {num_records}, Total Uncompressed Payload: {total_bytes / (1024*1024):.2f} MB")

    # Run warcio
    with tempfile.NamedTemporaryFile(suffix=".warc.gz") as out_warc:
        t0 = time.perf_counter()
        writer = WARCWriter(out_warc, gzip=True)

        # 1. Warcinfo record
        warcinfo_payload = (
            b"software: warcio 1.8.1\r\n"
            b"format: WARC File Format 1.1\r\n"
            b"conformsTo: http://bibnum.bnf.fr/WARC/WARC_Specification_ISO_28500_version_1.1.pdf\r\n"
        )
        warcinfo_headers = StatusAndHeaders("200 OK", [("Content-Type", "application/warc-fields")])
        wrec = writer.create_warcinfo_record(out_warc.name, {"software": "warcio 1.8.1"})
        writer.write_record(wrec)

        # 2. Response records
        for url, payload, mime in records_data:
            http_headers = StatusAndHeaders("200 OK", [
                ("Content-Type", mime),
                ("Content-Length", str(len(payload))),
                ("Server", "Baseline-Bench"),
            ])
            record = writer.create_warc_record(
                uri=url,
                record_type="response",
                payload=io.BytesIO(payload),
                http_headers=http_headers
            )
            writer.write_record(record)

        elapsed = time.perf_counter() - t0
        warc_size = out_warc.tell()

    throughput_mbs = (total_bytes / (1024 * 1024)) / elapsed
    records_per_sec = num_records / elapsed
    peak_rss = get_peak_rss_mb()

    print(f"warcio 1.8.1 Results:")
    print(f"  Elapsed: {elapsed:.3f} s")
    print(f"  Throughput: {throughput_mbs:.2f} MB/s ({records_per_sec:.1f} records/s)")
    print(f"  Compressed WARC Size: {warc_size / (1024*1024):.2f} MB")
    print(f"  Peak RSS: {peak_rss:.1f} MB")

    return {
        "tool": "warcio 1.8.1",
        "num_records": num_records,
        "total_mb": total_bytes / (1024 * 1024),
        "elapsed_s": elapsed,
        "throughput_mb_s": throughput_mbs,
        "records_per_sec": records_per_sec,
        "warc_size_mb": warc_size / (1024 * 1024),
        "peak_rss_mb": peak_rss
    }

def main():
    streams_dir = ROOT / "benchmarks/media/fixtures"
    generate_fixtures(streams_dir)

    # HTTP server for streams
    class StreamHandler(BaseHTTPRequestHandler):
        def do_GET(self):
            path = urlsplit(self.path).path
            if path.startswith("/streams/"):
                rel = path[len("/streams/"):]
                fpath = streams_dir / rel
                if fpath.exists() and fpath.is_file():
                    mime = "application/vnd.apple.mpegurl" if fpath.suffix == ".m3u8" else \
                           "application/dash+xml" if fpath.suffix == ".mpd" else \
                           "video/mp2t" if fpath.suffix == ".ts" else "video/iso.segment"
                    body = fpath.read_bytes()
                    self.send_response(200)
                    self.send_header("Content-Type", mime)
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
            self.send_response(404)
            self.end_headers()
        def log_message(self, *_): pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), StreamHandler)
    port = server.server_port
    base_url = f"http://127.0.0.1:{port}"
    th = threading.Thread(target=server.serve_forever, daemon=True)
    th.start()

    try:
        stream_res = benchmark_streams(base_url, streams_dir)
        warc_res = benchmark_warc()
        out = {
            "streams": stream_res,
            "warc": warc_res
        }
        out_file = ROOT / "benchmarks/media/baseline_results.json"
        out_file.write_text(json.dumps(out, indent=2))
        print(f"\nBaseline benchmark written to {out_file}")
    finally:
        server.shutdown()
        server.server_close()

if __name__ == "__main__":
    main()

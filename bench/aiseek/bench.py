"""Copy of AiSeek's ~/crawl-bench/bench.py, pointed at either server: Crawl4AI's document worker (~/crawl-bench/server.sh)
or pagesift's /v1/fetch adapter (bench/aiseek/server.sh). Set SERVER=crawl4ai|pagesift; results go to
bench/aiseek/results/<SERVER>/. URL sets are read from ~/crawl-bench. Changes from the original: the server script,
the results directory, and the browser process count also matching lightpanda/moli.

Phases (each writes JSON lines to results/<phase>-<tag>.jsonl and a resource trace to results/<phase>-<tag>-res.jsonl):
  pipeline  prod sample, 4 callers like document.fetch: http first (20 s), render when the page is short/blocked-looking (60 s)
  render    prod sample, every URL rendered (60 s), to compare with the http path
  sweep     fixed set repeated, CRAWL_PAGES in {1,4,8,16}, callers = 2 x pages (queueing like production), 240 requests each
  cold      first render after a fresh start, 5 restarts
Usage: bench.py <phase> [args]
"""

from __future__ import annotations

import asyncio
import csv
import json
import os
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

import httpx

BENCH = Path.home() / "crawl-bench"
SERVER = os.environ.get("SERVER", "pagesift")
SCRIPT = {"crawl4ai": BENCH / "server.sh", "pagesift": Path(__file__).resolve().parent / "server.sh"}[SERVER]
RES = Path(__file__).resolve().parent / "results" / SERVER
RES.mkdir(parents=True, exist_ok=True)
TOKEN = "bench-token-local-0123456789abcdef"
HTTP_MIN_CHARS = 1200
BARRIER = ("enable javascript", "verify you are human", "just a moment", "access denied", "are you a robot", "checking your browser", "captcha")


class Server:
    def __init__(self, pages: int, port: int, allow: str = "") -> None:
        self.pages, self.port, self.allow = pages, port, allow
        self.proc: subprocess.Popen | None = None
        self.samples: list[dict] = []
        self._stop = threading.Event()

    def start(self) -> float:
        t = time.monotonic()
        self.proc = subprocess.Popen([str(SCRIPT), str(self.pages), str(self.port), self.allow],
                                     stdout=open(RES / f"server-{self.port}.log", "a"), stderr=subprocess.STDOUT, start_new_session=True)
        while True:
            try:
                if httpx.get(f"http://127.0.0.1:{self.port}/health", timeout=1).status_code == 200:
                    break
            except httpx.HTTPError:
                pass
            if self.proc.poll() is not None:
                raise RuntimeError("server exited")
            time.sleep(0.1)
        ready = time.monotonic() - t
        self._cg = Path("/sys/fs/cgroup" + Path(f"/proc/{self.proc.pid}/cgroup").read_text().strip().split(":", 2)[2])
        threading.Thread(target=self._sample, daemon=True).start()
        return ready

    def _sample(self) -> None:
        while not self._stop.is_set():
            try:
                mem = int((self._cg / "memory.current").read_text())
                stat = dict(line.split() for line in (self._cg / "cpu.stat").read_text().splitlines())
                procs = (self._cg / "cgroup.procs").read_text().split()
                chrome = sum(1 for p in procs if any(k in _comm(p) for k in ("chrom", "lightpanda", "moli")))
                self.samples.append({"t": time.time(), "mem": mem, "cpu_us": int(stat["usage_usec"]), "procs": len(procs), "chrome": chrome})
            except (OSError, ValueError):
                pass
            time.sleep(0.5)

    def stop(self) -> None:
        self._stop.set()
        if self.proc and self.proc.poll() is None:
            os.killpg(self.proc.pid, signal.SIGTERM)
            try:
                self.proc.wait(20)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL)
        time.sleep(1)


def _comm(pid: str) -> str:
    try:
        return Path(f"/proc/{pid}/comm").read_text()
    except OSError:
        return ""


async def call(client: httpx.AsyncClient, port: int, url: str, mode: str, timeout_ms: int) -> dict:
    body = {"url": url, "mode": mode, "render": mode == "crawl4ai", "timeoutMs": timeout_ms}
    t = time.monotonic()
    try:
        r = await client.post(f"http://127.0.0.1:{port}/v1/fetch", json=body, headers={"x-internal-service-token": TOKEN},
                              timeout=timeout_ms / 1000 + 15)
        dt = time.monotonic() - t
        if r.status_code != 200:
            return {"mode": mode, "sec": dt, "http": r.status_code, "err": r.text[:200]}
        j = r.json()
        text = j.get("plainText") or ""
        return {"mode": mode, "sec": dt, "http": 200, "status": j.get("statusCode"), "chars": len(text.strip()), "md": len(j.get("markdown") or ""),
                "extraction": (j.get("metadata") or {}).get("extraction"), "barrier": any(b in text.lower()[:3000] for b in BARRIER)}
    except httpx.HTTPError as e:
        return {"mode": mode, "sec": time.monotonic() - t, "http": 0, "err": f"{type(e).__name__}: {e}"[:200]}


def verdict(r: dict) -> str:
    if r["http"] != 200:
        return "render"  # our side failed: the browser gets a try
    s = r["status"]
    if s in (401, 403, 451):
        return "blocked"
    if s in (404, 410) or (400 <= s < 500 and s != 429):
        return "gone"
    if s == 429:
        return "slow"
    if s >= 500:
        return "retry"
    if r["barrier"]:
        return "render"
    return "use" if r["chars"] >= HTTP_MIN_CHARS else "render"


def load_prod() -> list[tuple[str, str]]:
    return [(row[0], row[2]) for row in csv.reader(open(BENCH / "urls_prod.csv"))]


async def run_pool(items, worker, conc: int):
    q: asyncio.Queue = asyncio.Queue()
    for it in items:
        q.put_nowait(it)
    out = []

    async def loop():
        while not q.empty():
            it = q.get_nowait()
            out.append(await worker(it))

    await asyncio.gather(*(loop() for _ in range(conc)))
    return out


def dump(name: str, rows: list[dict], srv: Server) -> None:
    with open(RES / f"{name}.jsonl", "w") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    with open(RES / f"{name}-res.jsonl", "w") as f:
        for s in srv.samples:
            f.write(json.dumps(s) + "\n")


async def phase_pipeline(port=8101):
    srv = Server(4, port)
    srv.start()
    try:
        async with httpx.AsyncClient() as c:
            async def w(it):
                grp, url = it
                t = time.monotonic()
                h = await call(c, port, url, "http", 20_000)
                v = verdict(h)
                row = {"grp": grp, "url": url, "http_step": h, "verdict": v}
                if v == "render":
                    row["render_step"] = await call(c, port, url, "crawl4ai", 60_000)
                row["total"] = time.monotonic() - t
                return row
            t0 = time.time()
            rows = await run_pool(load_prod(), w, 4)
            print("pipeline wall", round(time.time() - t0, 1))
        dump("pipeline", rows, srv)
    finally:
        srv.stop()


async def phase_render(port=8102):
    srv = Server(4, port)
    srv.start()
    try:
        async with httpx.AsyncClient() as c:
            async def w(it):
                grp, url = it
                return {"grp": grp, "url": url, **(await call(c, port, url, "crawl4ai", 60_000))}
            t0 = time.time()
            rows = await run_pool(load_prod(), w, 4)
            print("render wall", round(time.time() - t0, 1))
        dump("render", rows, srv)
    finally:
        srv.stop()


async def phase_sweep(pages: int, n=240, port=8103):
    urls = [u.strip() for u in open(BENCH / "urls_fixed.txt") if u.strip() and not u.startswith("#")]
    items = [urls[i % len(urls)] for i in range(n)]
    srv = Server(pages, port)
    srv.start()
    try:
        async with httpx.AsyncClient(limits=httpx.Limits(max_connections=100)) as c:
            async def w(url):
                r = await call(c, port, url, "crawl4ai", 60_000)
                return {"url": url, "end": time.time(), **r}
            t0 = time.time()
            rows = await run_pool(items, w, 2 * pages)
            wall = time.time() - t0
            print(f"sweep pages={pages} wall={wall:.1f}s")
        dump(f"sweep-p{pages}", rows + [{"wall": wall}], srv)
    finally:
        srv.stop()


async def phase_cold(rounds=5, port=8104):
    url = "https://example.com/"
    rows = []
    for _ in range(rounds):
        srv = Server(4, port)
        ready = srv.start()
        async with httpx.AsyncClient() as c:
            first = await call(c, port, url, "crawl4ai", 60_000)
            second = await call(c, port, url, "crawl4ai", 60_000)
            h = await call(c, port, url, "http", 20_000)
        rows.append({"ready": ready, "first_render": first["sec"], "second_render": second["sec"], "http": h["sec"]})
        print(rows[-1])
        srv.stop()
    dump("cold", rows, srv)


if __name__ == "__main__":
    ph = sys.argv[1]
    if ph == "pipeline":
        asyncio.run(phase_pipeline())
    elif ph == "render":
        asyncio.run(phase_render())
    elif ph == "sweep":
        for p in [int(x) for x in sys.argv[2:]] or [1, 4, 8, 16]:
            asyncio.run(phase_sweep(p))
    elif ph == "cold":
        asyncio.run(phase_cold())

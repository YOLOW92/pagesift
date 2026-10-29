#!/usr/bin/env python3
"""Restore the frozen 0.81 benchmark inputs without changing the recorded evidence.

    python3 -I restore_inputs.py a1 a4 b3 b5 b2 [--out ~/mix-tools/inputs] [--force]

- a1, a4, b3: reuse benchmarks/mix/materialize.py with MIX_DATA=benchmarks/data.
  Missing sources are recorded, not replaced (A4 has 15 known missing block pages).
- b5: the 134 old_scans / old_scans_math PDFs are local; symlink them and verify
  the SHA-256 recorded in benchmarks/mix/v2/samples/b5.inputs.tsv.
- b2: only the 71 rows whose old fetch succeeded are restored.  The recorded
  final URL is tried first, then the original URL; a file is accepted only when
  its SHA-256 equals the old one.  Nothing in benchmarks/mix/samples is modified.

Every subset gets <out>/<sub>.manifest (id<TAB>path<TAB>url) for driver.py and
a verification table in $RUN/manifests/<sub>.inputs.tsv.
"""
import argparse
import csv
import hashlib
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
MIX = Path(os.environ.get("MIX_TOOLS", Path.home() / "mix-tools"))
RUN = Path(os.environ.get("RUN", MIX / "runs-081" / "manual"))
OUT = MIX / "inputs"
UA = "pagesift-mix-bench/0.81 (+https://github.com/YOLOW92/pagesift)"


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def rows(path):
    with open(path, encoding="utf-8") as f:
        return list(csv.DictReader(f, delimiter="\t"))


def write_manifest(sub, items):
    p = OUT / f"{sub}.manifest"
    p.parent.mkdir(parents=True, exist_ok=True)
    with open(p, "w", encoding="utf-8") as f:
        for id_, path, url in items:
            f.write(f"{id_}\t{path}\t{url or ''}\n")


def write_table(sub, items, old_sha=None):
    d = RUN / "manifests"
    d.mkdir(parents=True, exist_ok=True)
    p = d / f"{sub}.inputs.tsv"
    old_sha = old_sha or {}
    with open(p, "w", encoding="utf-8") as f:
        f.write("id\tpath\tbytes\tsha256\tmatches_recorded\n")
        for id_, path, _url in items:
            b = Path(path).read_bytes()
            h = hashlib.sha256(b).hexdigest()
            match = "" if id_ not in old_sha else ("yes" if h == old_sha[id_] else "NO")
            f.write(f"{id_}\t{path}\t{len(b)}\t{h}\t{match}\n")
    return p


def materialize_sub(sub, samples_dir):
    """a1 / a4 / b3 via the frozen materializer, recording missing sources."""
    os.environ["MIX_DATA"] = str(REPO / "benchmarks/data")
    os.environ["MIX_OFFICE"] = str(REPO / "tasks/0.80/fix3/office")
    os.environ["MIX_TASKS"] = str(REPO / "tasks/0.80")
    sys.path.insert(0, str(REPO / "benchmarks/mix"))
    import materialize as M  # noqa: E402

    src = Path(samples_dir) / f"{sub}.tsv"
    data = rows(src)
    dest = OUT / sub
    dest.mkdir(parents=True, exist_ok=True)
    items, missing = [], []
    old = {}
    oldp = Path(samples_dir) / f"{sub}.inputs.tsv"
    if oldp.exists():
        old = {r["id"]: r["sha256"] for r in rows(oldp)}
    for r in data:
        try:
            p = M.build(r, sub, OUT)
            b = p.read_bytes()
        except Exception as e:  # noqa: BLE001  missing source is data, not a crash
            missing.append((r["id"], r.get("location", ""), str(e)[:160]))
            continue
        items.append((r["id"], str(p), r.get("orig_url", "")))
        if r["id"] in old and hashlib.sha256(b).hexdigest() != old[r["id"]]:
            print(f"{sub}: SHA-256 differs from recorded input: {r['id']}", file=sys.stderr)
    write_manifest(sub, items)
    table = write_table(sub, items, old)
    print(f"{sub}: {len(items)}/{len(data)} restored, {len(missing)} missing; {table}")
    if missing:
        (RUN / f"{sub}.missing.tsv").write_text(
            "id\tlocation\treason\n" + "".join(f"{i}\t{l}\t{e}\n" for i, l, e in missing),
            encoding="utf-8")
    return len(missing)


def b5(samples_dir):
    src = Path(samples_dir) / "b5.tsv"
    old = {r["id"]: r["sha256"] for r in rows(Path(samples_dir) / "b5.inputs.tsv")}
    dest = OUT / "b5"
    dest.mkdir(parents=True, exist_ok=True)
    items, missing, bad = [], [], []
    for r in rows(src):
        source = REPO / "benchmarks/data" / r["location"]
        if not source.exists():
            missing.append((r["id"], r["location"]))
            continue
        h = sha(source)
        if h != old[r["id"]]:
            bad.append((r["id"], h, old[r["id"]]))
            continue
        p = dest / f"{r['id']}.pdf"
        if p.is_symlink() or p.exists():
            p.unlink()
        p.symlink_to(source.resolve())
        items.append((r["id"], str(p), r.get("orig_url", "")))
    write_manifest("b5", items)
    table = write_table("b5", items, old)
    print(f"b5: {len(items)}/{len(rows(src))} restored, {len(missing)} missing, {len(bad)} SHA mismatches; {table}")
    if missing or bad:
        (RUN / "b5.missing.tsv").write_text(
            "id\treason\n" + "".join(f"{i}\tmissing {l}\n" for i, l in missing)
            + "".join(f"{i}\tSHA mismatch {h} != {o}\n" for i, h, o in bad), encoding="utf-8")
    return len(missing) + len(bad)


class HostGate:
    def __init__(self):
        self.lock = threading.Lock()
        self.gates = {}
        self.last = {}

    def wait(self, host):
        with self.lock:
            g = self.gates.setdefault(host, threading.Lock())
        with g:
            gap = 3.0 if host.endswith("arxiv.org") else 1.0
            delay = self.last.get(host, 0) + gap - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            self.last[host] = time.monotonic()


def download(url, gate, attempts=4):
    host = urllib.parse.urlsplit(url).hostname or ""
    for i in range(attempts):
        gate.wait(host)
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "*/*"})
            with urllib.request.urlopen(req, timeout=90) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            if e.code in (429, 500, 502, 503, 504) and i + 1 < attempts:
                time.sleep(5 * (i + 1))
                continue
            raise
        except Exception:
            if i + 1 < attempts:
                time.sleep(3 * (i + 1))
                continue
            raise


def b2(force=False):
    fetched = rows(REPO / "benchmarks/mix/samples/b2.fetched.tsv")
    sample = {r["id"]: r for r in rows(REPO / "benchmarks/mix/samples/b2.tsv")}
    dest = OUT / "b2"
    dest.mkdir(parents=True, exist_ok=True)
    gate = HostGate()
    todo = [r for r in fetched if r["status"] == "ok"]
    print(f"b2: {len(todo)}/{len(fetched)} recorded successes to restore", flush=True)
    lock = threading.Lock()
    items, results = [], []

    def one(r):
        id_ = r["id"]
        want = r["sha256"]
        p = dest / f"{id_}.pdf"
        if not force and p.exists() and sha(p) == want:
            with lock:
                items.append((id_, str(p), r.get("final_url", "")))
                results.append((id_, "cached", "", len(p.read_bytes()), want))
            return
        errors = []
        urls = [u for u in (r.get("final_url"), sample.get(id_, {}).get("location")) if u]
        for url in dict.fromkeys(urls):
            try:
                body = download(url, gate)
            except Exception as e:  # noqa: BLE001
                errors.append(f"{url}: {type(e).__name__}: {e}")
                continue
            got = hashlib.sha256(body).hexdigest()
            if got != want:
                errors.append(f"{url}: SHA {got[:16]} != {want[:16]} ({len(body)} bytes)")
                continue
            tmp = p.with_suffix(".part")
            tmp.write_bytes(body)
            tmp.replace(p)
            with lock:
                items.append((id_, str(p), url))
                results.append((id_, "ok", url, len(body), got))
            print(f"b2: {len(items)}/{len(todo)} {id_}", flush=True)
            return
        with lock:
            results.append((id_, "unavailable", "; ".join(errors)[-500:], 0, ""))
        print(f"b2: FAILED {id_}: {errors[-1] if errors else 'no URL'}", file=sys.stderr, flush=True)

    with ThreadPoolExecutor(8) as ex:
        list(ex.map(one, todo))
    items.sort(key=lambda x: x[0])
    results.sort(key=lambda x: x[0])
    write_manifest("b2", items)
    table = RUN / "manifests"
    table.mkdir(parents=True, exist_ok=True)
    with open(table / "b2.restored.tsv", "w", encoding="utf-8") as f:
        f.write("id\tstatus\turl\tbytes\tsha256\n")
        for row in results:
            f.write("\t".join(str(x) for x in row) + "\n")
    print(f"b2: restored {len(items)}/{len(todo)}; {table / 'b2.restored.tsv'}")
    return len(todo) - len(items)


def main():
    global OUT
    ap = argparse.ArgumentParser()
    ap.add_argument("subs", nargs="+", choices=["a1", "a4", "b3", "b5", "b2"])
    ap.add_argument("--out", type=Path, default=OUT)
    ap.add_argument("--samples", type=Path, default=REPO / "benchmarks/mix/samples")
    ap.add_argument("--force", action="store_true", help="b2: re-download even if the cached file matches")
    a = ap.parse_args()
    OUT = a.out
    OUT.mkdir(parents=True, exist_ok=True)
    RUN.mkdir(parents=True, exist_ok=True)
    bad = 0
    for sub in a.subs:
        if sub == "b2":
            bad += b2(a.force)
        elif sub == "b5":
            bad += b5(REPO / "benchmarks/mix/v2/samples")
        else:
            n = materialize_sub(sub, a.samples)
            # A4 has 15 documented missing block pages in the frozen v1 sample
            # (samples/a4-missing.tsv); they are expected, not a restore error.
            bad += max(0, n - 15) if sub == "a4" else n
    if bad:
        print(f"restore_inputs: {bad} inputs missing or mismatched", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""逐项比较 Chrome 和 pagesift 的录制结果。

    compare.py results/<日期>

每个协议取第一条完整的记录。除下面列出的“本来就随机”的项，任何不同都算不一致，退出码 1。
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from ja4 import summarize  # noqa: E402

# 档案有意改掉的请求头（见 fingerprint.md 的已知差异）：录制用的是无头版，档案写普通版的值。
HEADLESS = {"user-agent": ("HeadlessChrome/", "Chrome/")}
# 本来就每连接不同，不比较：扩展的线上顺序、ECH payload 的长度（144/176/208/240 随机）、authority 无关项
SKIP_DETAILS = {"fe0d ech_outer"}


def first(path):
    records = [json.loads(line) for line in path.read_text().splitlines()]
    for rec in records:
        if not rec.get("error") and (rec.get("requests") or rec.get("h1_requests")):
            return rec
    return records[0] if records else None


def row(name, chrome, ours, problems):
    same = chrome == ours
    print(f"{'same' if same else 'DIFF'}  {name}")
    if not same:
        print(f"      chrome:   {chrome}")
        print(f"      pagesift: {ours}")
        problems.append(name)
    elif len(str(chrome)) < 100:
        print(f"      {chrome}")


def compare(out, tag, problems):
    a, b = first(out / f"chrome-{tag}.jsonl"), first(out / f"pagesift-{tag}.jsonl")
    print(f"\n=== {tag} ===")
    if not a or not b:
        print(f"DIFF  no record (chrome: {bool(a)}, pagesift: {bool(b)})")
        problems.append(f"{tag}: missing record")
        return
    sa, sb = summarize(a), summarize(b)
    if "error" in sb:
        print(f"      pagesift record error: {sb['error']}")
    for key in ("record_version", "legacy_version", "session_id_len", "suites", "compression", "ext_set"):
        row(key, sa[key], sb[key], problems)
    row("JA4", sa["ja4"]["ja4"], sb["ja4"]["ja4"], problems)
    row("JA4_r", sa["ja4"]["ja4_r"], sb["ja4"]["ja4_r"], problems)
    keys = sorted(set(sa["details"]) | set(sb["details"]))
    for key in keys:
        if key in SKIP_DETAILS:
            continue
        row(f"ext {key}", sa["details"].get(key), sb["details"].get(key), problems)
    for label, s in (("chrome", sa), ("pagesift", sb)):
        ech = s["details"].get("fe0d ech_outer")
        print(f"info  ECH GREASE ({label}): {ech}")
        order = s["ext_wire_order"]
        print(f"info  wire order ({label}): {order}")
    row(
        "negotiated",
        {k: sa["negotiated"][k] for k in ("tls_version", "alpn")},
        {k: sb["negotiated"][k] for k in ("tls_version", "alpn")},
        problems,
    )
    print(f"info  cipher chrome={sa['negotiated']['cipher']} pagesift={sb['negotiated']['cipher']}")
    if "http2" in sa or "http2" in sb:
        ha, hb = sa.get("http2", {}), sb.get("http2", {})
        for key in ("akamai", "headers_flags", "headers_priority", "first_frame_types", "header_order"):
            row(f"h2 {key}", ha.get(key), hb.get(key), problems)
        for name in sorted(set(ha.get("header_values", {})) | set(hb.get("header_values", {}))):
            va = ha.get("header_values", {}).get(name)
            vb = hb.get("header_values", {}).get(name)
            if name in HEADLESS and va:
                va = va.replace(*HEADLESS[name])
            row(f"h2 header {name}", va, vb, problems)
    if "http1" in sa or "http1" in sb:
        la = sa.get("http1", {}).get("headers", [])
        lb = sb.get("http1", {}).get("headers", [])
        row("h1 request_line", sa.get("http1", {}).get("request_line"), sb.get("http1", {}).get("request_line"), problems)
        row("h1 header names", [x.split(":")[0] for x in la], [x.split(":")[0] for x in lb], problems)
        for x, y in zip(la, lb):
            x = x.replace("HeadlessChrome/", "Chrome/")
            row(f"h1 line {x.split(':')[0]}", x, y, problems)


def main():
    out = Path(sys.argv[1])
    problems = []
    for tag in ("h2", "http1.1"):
        compare(out, tag, problems)
    print(f"\n{'MATCH' if not problems else 'MISMATCH: ' + ', '.join(problems)}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

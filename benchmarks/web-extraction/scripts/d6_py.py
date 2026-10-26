#!/usr/bin/env python3
"""D6 编码识别竞品适配器。用法: d6_py.py --tool X --manifest m.jsonl --out o.jsonl"""
import argparse, json, sys

def detect(tool, data):
    if tool == "chardet":
        import chardet
        r = chardet.detect(data)
        return (r.get("encoding") or "unknown")
    if tool == "charset-normalizer":
        from charset_normalizer import from_bytes
        r = from_bytes(data).best()
        return str(r.encoding) if r and r.encoding else "unknown"
    if tool == "cchardet":
        import cchardet
        return (cchardet.detect(data).get("encoding") or "unknown")
    if tool == "uchardet":
        import subprocess, tempfile, os
        with tempfile.NamedTemporaryFile(delete=False) as t:
            t.write(data); tmp = t.name
        try:
            out = subprocess.run(["uchardet", tmp], capture_output=True, text=True)
            return (out.stdout or "unknown").strip() or "unknown"
        finally:
            os.unlink(tmp)
    return "unknown"

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tool", required=True)
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    n = 0
    with open(a.out, "w", encoding="utf-8") as o:
        for line in open(a.manifest, encoding="utf-8"):
            if not line.strip(): continue
            r = json.loads(line)
            try:
                data = open(r["path"], "rb").read()
                enc = detect(a.tool, data)
            except Exception as e:
                enc = "error"
            o.write(json.dumps({"id": r["id"], "detected": enc}, ensure_ascii=False) + "\n")
            n += 1
    print(json.dumps({"tool": a.tool, "n": n}), file=sys.stderr)

if __name__ == "__main__":
    main()

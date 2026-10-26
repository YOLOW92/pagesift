#!/usr/bin/env python3
"""把 robotstxt-spec-test 的 textproto 解析成 JSONL 供 PageSift 判定。

textproto 结构简单：tests:[{robotstxt:"..." test_expectations:[{testurl:.. useragent:.. expected_outcome:..}]}]
字符串可有相邻拼接。这里按块扫描，不做完整 protobuf 解析。
"""
import json, re, sys
from pathlib import Path

def unescape(s: str) -> str:
    return (s.replace('\\\\', '\x00').replace('\\n', '\n').replace('\\t', '\t')
             .replace('\\"', '"').replace('\x00', '\\'))

def strings_after(block: str, key: str):
    """取 key: 之后连续的带引号字符串（相邻即可拼接）。"""
    m = re.search(rf'\b{key}\s*:\s*', block)
    if not m:
        return None
    i = m.end()
    parts = []
    while i < len(block):
        while i < len(block) and block[i] in ' \t\r\n':
            i += 1
        if i >= len(block) or block[i] != '"':
            break
        i += 1
        buf = []
        while i < len(block):
            c = block[i]
            if c == '\\':
                buf.append(block[i:i+2]); i += 2; continue
            if c == '"':
                i += 1; break
            buf.append(c); i += 1
        parts.append(unescape(''.join(buf)))
    return ''.join(parts) if parts else None

def main():
    src = Path(sys.argv[1]); out = Path(sys.argv[2])
    rows = []
    for f in sorted(src.glob("*.textproto")):
        t = f.read_text(encoding="utf-8")
        # 切出每个 test 块：以 robotstxt 出现位置为界
        idxs = [m.start() for m in re.finditer(r'\brobotstxt\s*:', t)]
        for n, start in enumerate(idxs):
            end = idxs[n + 1] if n + 1 < len(idxs) else len(t)
            block = t[start:end]
            rt = strings_after(block, "robotstxt")
            if rt is None:
                continue
            for m in re.finditer(r'\{([^{}]*?testurl[^{}]*?)\}', block, re.S):
                body = m.group(1)
                url = strings_after(body, "testurl")
                ua = strings_after(body, "useragent")
                eo = re.search(r'expected_outcome\s*:\s*([A-Z_]+)', body)
                if not url or ua is None or not eo:
                    continue
                rows.append({"file": f.name, "robotstxt": rt, "url": url,
                             "useragent": ua, "expected": eo.group(1)})
    out.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n", encoding="utf-8")
    print(f"cases={len(rows)} from {len(list(src.glob('*.textproto')))} files")

if __name__ == "__main__":
    main()

"""格式冒烟：真跑 CLI，确认每个代码语言扩展名都进代码块，SQLite 的内容和真实数据库逐行一致。

    python3 -I benchmarks/formats/smoke.py --pagesift target/quick/pagesift

SQLite 文件用 Python 标准库生成（真实的 SQLite 引擎写的），结果和 SELECT 读出的对照。
"""
import argparse
import re
import sqlite3
import subprocess
import tempfile
from pathlib import Path


def scrape(binary, path):
    r = subprocess.run([binary, "scrape", str(path)], capture_output=True, timeout=60)
    return r.returncode, r.stdout.decode("utf-8", "replace"), r.stderr.decode("utf-8", "replace")


def code_languages(binary, tmp):
    src = Path(__file__).resolve().parents[2] / "crates/pagesift-office/src/code.rs"
    text = src.read_text(encoding="utf-8")
    body = text[text.index("Some(match ext {") : text.index("/// 没有扩展名的常见文件")]
    table = {}
    for pats, lang in re.findall(r'((?:"[^"]+"(?:\s*\|\s*)?)+)\s*=>\s*"([^"]+)",', body):
        for ext in re.findall(r'"([^"]+)"', pats):
            table[ext] = lang
    bad = []
    marker = "pagesift_smoke_marker_42"
    for ext, lang in sorted(table.items()):
        path = tmp / f"sample.{ext}"
        path.write_text(f"{marker} = 1\n", encoding="utf-8")
        code, out, err = scrape(binary, path)
        path.unlink()
        if code != 0 or marker not in out or "```" not in out:
            bad.append((ext, lang, code, (out + err)[:80]))
    tail = text[text.index("/// 没有扩展名的常见文件") :]
    tail = tail[: tail.index("#[cfg(test)]")]
    names = {}
    for pats, lang in re.findall(r'((?:"[^"]+"(?:\s*\|\s*)?)+)\s*=>\s*"([^"]+)",', tail):
        for name in re.findall(r'"([^"]+)"', pats):
            names[name] = lang
    for name, lang in sorted(names.items()):
        path = tmp / name
        path.write_text(f"{marker} = 1\n", encoding="utf-8")
        code, out, err = scrape(binary, path)
        path.unlink()
        if code != 0 or marker not in out or "```" not in out:
            bad.append((name, lang, code, (out + err)[:80]))
    print(f"代码语言：{len(set(table.values()) | set(names.values()))} 种，{len(table)} 个扩展名，{len(names)} 个文件名，失败 {len(bad)}")
    for b in bad[:20]:
        print("  失败", b)
    return not bad


def sqlite_truth(binary, tmp):
    ok = True
    for name, pragma in (("utf8", "UTF-8"), ("utf16", "UTF-16le"), ("be", "UTF-16be")):
        path = tmp / f"{name}.sqlite"
        db = sqlite3.connect(path)
        db.execute(f"PRAGMA encoding='{pragma}'")
        db.execute("PRAGMA page_size=1024")
        db.execute("CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT, note TEXT, score REAL, pic BLOB)")
        db.execute("CREATE TABLE kv(v TEXT, k TEXT PRIMARY KEY) WITHOUT ROWID")
        db.execute("CREATE INDEX idx ON people(name)")
        rows = [(i * 3, f"名字{i}", "长文本" * (i % 900), i / 4, b"xx") for i in range(1, 3000)]
        db.executemany("INSERT INTO people VALUES(?,?,?,?,?)", rows)
        db.executemany("INSERT INTO kv VALUES(?,?)", [(f"v{i}", f"k{i:05}") for i in range(2000)])
        db.commit()
        db.close()
        code, out, err = scrape(binary, path)
        missing = 0
        for r in rows:
            if f"| {r[0]} | {r[1]} |" not in out or (r[2] and r[2] not in out):
                missing += 1
        kv_missing = sum(1 for i in range(2000) if f"| v{i} | k{i:05} |" not in out)
        print(f"SQLite {pragma}：退出码 {code}，people 缺 {missing}/{len(rows)}，kv 缺 {kv_missing}/2000，输出 {len(out)} 字节")
        ok = ok and code == 0 and not missing and not kv_missing
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pagesift", required=True)
    a = ap.parse_args()
    with tempfile.TemporaryDirectory() as t:
        tmp = Path(t)
        results = [code_languages(a.pagesift, tmp), sqlite_truth(a.pagesift, tmp)]
    raise SystemExit(0 if all(results) else 1)


if __name__ == "__main__":
    main()

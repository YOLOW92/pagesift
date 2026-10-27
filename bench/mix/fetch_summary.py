"""把 samples/<子类>.fetched.tsv 汇成“抓到多少、每类失败各多少”的 Markdown 表。
    python3 -I bench/mix/fetch_summary.py a2 b2 > bench/mix/samples/fetch-summary.md
"""
import re
import sys
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent

# 按顺序匹配，取第一个；“仍失败”的补抓记录按其中的原始原因归类
CLASSES = [
    ("robots.txt 禁止", r"robots\.txt 禁止"),
    ("裸域名查不到地址", r"\[Errno -5\]|\[Errno -2\]"),
    ("超时、DNS 暂时失败、连接被重置", r"timed out|Temporary failure in name resolution|Connection reset|TimeoutError"),
    ("HTTP 401/402/403/407（登录、付费、拒绝）", r"HTTP 40[1237]"),
    ("其他 HTTP 错误", r"HTTP \d{3}"),
    ("证书或 TLS 错误", r"SSL|CERTIFICATE|TLSV1"),
    ("连接被拒绝、无路由", r"Connection refused|No route to host"),
    ("首页上没有同站链接", r"首页上没有同站链接"),
    ("页面上没有 PDF 附件", r"没有 PDF 附件"),
    ("不是 PDF", r"不是 PDF"),
    ("超过大小上限", r"超过 \d+ MB"),
    ("重定向过多", r"重定向超过"),
]


def classify(reason):
    for name, pat in CLASSES:
        if re.search(pat, reason):
            return name
    return "其他"


for sub in sys.argv[1:]:
    lines = (HERE / "samples" / f"{sub}.fetched.tsv").read_text(encoding="utf-8").splitlines()
    cols = lines[0].split("\t")
    rows = [dict(zip(cols, l.split("\t"))) for l in lines[1:] if l]
    ok = [r for r in rows if r["status"] == "ok"]
    patched = sum(1 for r in ok if r["reason"].startswith("冻结后补抓"))
    c = Counter(classify(r["reason"]) for r in rows if r["status"] != "ok")
    print(f"### {sub.upper()}：抓到 {len(ok)} / {len(rows)}（越高越好）\n")
    if patched:
        print(f"其中 {patched} 份是按冻结后补的抓取规则补到的（见 RULES 第七节）。\n")
    print("| 不可用的原因 | 份数 |\n|---|---|")
    for name, n in c.most_common():
        print(f"| {name} | {n} |")
    print(f"| **合计不可用** | **{len(rows) - len(ok)}** |\n")

#!/usr/bin/env python3
"""Deterministic fixtures for the 0.80.1 performance comparison.

PDF: N pages of an arXiv-style paper: a big title, an author line whose names carry
superscript footnote markers from a CMSY (math symbol) font, ~45 body lines and 3 formula lines
per page. CSVs: ASCII, Shift_JIS, GBK and UTF-8 CJK tables.
"""
import os
import sys

OUT = sys.argv[1] if len(sys.argv) > 1 else "."
PAGES = int(sys.argv[2]) if len(sys.argv) > 2 else 40


def pdf_literal(s: str) -> str:
    return "(" + s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)") + ")"


def page_content(n: int) -> str:
    c = []
    c.append(f"BT /F1 20 Tf 1 0 0 1 72 740 Tm {pdf_literal('Attention Is All You Need' if n == 1 else f'Section {n} of the paper')} Tj ET\n")
    # author line: names in Helvetica 12 pt, markers in CMSY 8 pt raised 3.6 pt
    x = 72.0
    y = 712.0
    # 每页的作者名都不同，免得被当成页眉去掉
    authors = [("Ashish Vaswani", "*"), ("Noam Shazeer", "*"), ("Niki Parmar", "y"), ("Jakob Uszkoreit", "*")]
    for name, marker in [(f"{a} {chr(65 + (n + k) % 26)}", m) if n > 1 else (a, m) for k, (a, m) in enumerate(authors)]:
        c.append(f"BT /F1 12 Tf 1 0 0 1 {x:.1f} {y} Tm {pdf_literal(name)} Tj ET\n")
        w = 12 * 0.5 * len(name)
        c.append(f"BT /F2 8 Tf 1 0 0 1 {x + w:.1f} {y + 3.6} Tm {pdf_literal(marker)} Tj ET\n")
        x += w + 8 + 14
    y = 690.0
    for i in range(45):
        text = (f"Paragraph line {i} on page {n} explains the model in prose, with commas, "
                f"clauses and enough words to look like an article body that a reader keeps.")
        c.append(f"BT /F1 10 Tf 1 0 0 1 72 {y:.1f} Tm {pdf_literal(text)} Tj ET\n")
        y -= 12.0
        if i % 15 == 14:
            # formula line: x <= y + 2^N
            c.append(f"BT /F1 10 Tf 1 0 0 1 72 {y:.1f} Tm (x) Tj ET\n")
            c.append(f"BT /F2 10 Tf 1 0 0 1 80 {y:.1f} Tm (\\024) Tj ET\n")
            c.append(f"BT /F1 10 Tf 1 0 0 1 90 {y:.1f} Tm (y + 2) Tj ET\n")
            c.append(f"BT /F1 7 Tf 1 0 0 1 112 {y + 3.6:.1f} Tm (N) Tj ET\n")
            y -= 12.0
    return "".join(c)


def make_pdf(path: str, pages: int, with_title: bool = True) -> None:
    objs = []
    objs.append("<< /Type /Catalog /Pages 2 0 R >>")
    kids = " ".join(f"{6 + 2 * i} 0 R" for i in range(pages))
    objs.append(f"<< /Type /Pages /Count {pages} /Kids [{kids}] >>")
    objs.append("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
    objs.append("<< /Type /Font /Subtype /Type1 /BaseFont /CMSY10 "
                "/Encoding << /Type /Encoding /Differences [20 /lessequal 42 /asteriskmath 121 /dagger] >> >>")
    objs.append("<< /Producer (gen_fixtures) >>" if not with_title else "<< /Title (Attention Is All You Need) >>")
    for i in range(pages):
        content = page_content(i + 1)
        objs.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                    f"/Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {7 + 2 * i} 0 R >>")
        objs.append(f"<< /Length {len(content.encode())} >>\nstream\n{content}\nendstream")
    out = bytearray(b"%PDF-1.7\n")
    offsets = []
    for i, o in enumerate(objs):
        offsets.append(len(out))
        out += f"{i + 1} 0 obj\n{o}\nendobj\n".encode()
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        out += f"{off:010} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    with open(path, "wb") as f:
        f.write(out)


def make_csvs() -> None:
    rows = 60000
    with open(os.path.join(OUT, "ascii.csv"), "w", encoding="utf-8", newline="") as f:
        f.write("id,name,qty,price,city,note,flag,ts\n")
        for i in range(rows):
            f.write(f'{i},"Item, {i}",{i % 97},{i * 0.37:.2f},Springfield,"a ""quoted"" note",{i % 2},2026-10-27T12:00:{i % 60:02}\n')
    ja = ["東京", "大阪", "名古屋", "札幌", "福岡"]
    with open(os.path.join(OUT, "sjis.csv"), "wb") as f:
        f.write("番号,都市,数量,備考\n".encode("shift_jis"))
        for i in range(rows // 2):
            f.write(f"{i},{ja[i % 5]},{i % 50},注文番号 {i} の備考です\n".encode("shift_jis"))
    zh = ["北京", "上海", "广州", "深圳", "成都"]
    with open(os.path.join(OUT, "gbk.csv"), "wb") as f:
        f.write("编号,城市,数量,备注\n".encode("gbk"))
        for i in range(rows // 2):
            f.write(f"{i},{zh[i % 5]},{i % 50},订单 {i} 的备注信息\n".encode("gbk"))
    with open(os.path.join(OUT, "utf8-cjk.csv"), "w", encoding="utf-8", newline="") as f:
        f.write("编号,城市,数量,备注\n")
        for i in range(rows // 2):
            f.write(f"{i},{zh[i % 5]},{i % 50},订单 {i} 的备注信息\n")


os.makedirs(OUT, exist_ok=True)
make_pdf(os.path.join(OUT, "paper.pdf"), PAGES)
make_pdf(os.path.join(OUT, "paper-untitled.pdf"), 3, with_title=False)
make_csvs()
print("ok")

"""G1：pagesift 的 CCITT 解码对照参照图（libtiff 编码前的原图，或 pdfimages -png 解出的图）。

    python3 -I benchmarks/history/1.0/g1/compare_ccitt.py <参照 png 目录> <ccitt_check 输出目录> <ccitt_check 的 tsv>

黑白图没有容差：逐像素必须相同。参照图整张反色时（pdfimages 按 /Decode 输出）记为“反色一致”。
"""
import sys
from pathlib import Path
from PIL import Image

ref, out, tsv = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
rows = [l.split('\t') for l in tsv.read_text().splitlines()]
same, inverted, bad, px, ms = 0, 0, [], 0, 0.0
for r in rows:
    name = r[0]
    if r[1] == 'ERROR':
        bad.append((name, 'error ' + r[2]))
        continue
    w, h = int(r[1]), int(r[2])
    ms += float(r[3])
    im = Image.open(ref / f'{name}.png').convert('L')
    if im.size != (w, h):
        bad.append((name, f'尺寸不同 {im.size} vs {(w, h)}'))
        continue
    want = im.point(lambda v: 255 if v >= 128 else 0).tobytes()
    ours = (out / f'{name}.y').read_bytes()
    px += w * h
    if ours == want:
        same += 1
    elif ours == bytes(255 - v for v in want):
        inverted += 1
    else:
        diff = sum(1 for a, b in zip(ours, want) if a != b)
        first = next(i for i, (a, b) in enumerate(zip(ours, want)) if a != b)
        bad.append((name, f'{diff} 个像素不同，第一个在行 {first // w} 列 {first % w}'))
print(f'{len(rows)} 张，一致 {same}，反色一致 {inverted}，不一致 {len(bad)}；{px} 像素，解码共 {ms:.1f} ms')
for b in bad[:20]:
    print(*b, sep='\t')

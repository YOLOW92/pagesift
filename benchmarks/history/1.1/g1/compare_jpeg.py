"""G1：pagesift 的 JPEG 解码对照 libjpeg-turbo（经 Pillow）。

    python3 -I benchmarks/history/1.1/g1/compare_jpeg.py <jpg 目录> <jpeg_check 输出目录> <jpeg_check 的 tsv>

3 分量的比 Y 通道（Pillow 以 YCbCr 模式解出、不做色彩变换），其余比灰度。
误差上限 2：IEEE 1180-1990 规定 IDCT 每像素峰值误差不超过 1，两边各 1。
"""
import sys, time
from pathlib import Path
from PIL import Image

src, out, tsv = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
TOL = 2
rows = [l.split('\t') for l in tsv.read_text().splitlines()]
worst, bad, total_px, over, ref_ms = 0, [], 0, 0, 0.0
for r in rows:
    name = r[0]
    if r[1] == 'ERROR':
        bad.append((name, 'error'))
        continue
    w, h, c = int(r[1]), int(r[2]), int(r[3])
    t = time.perf_counter()
    im = Image.open(src / f'{name}.jpg')
    if c == 3:
        im.draft('YCbCr', im.size)
        ref = im.tobytes()[0::3]
    else:
        ref = im.convert('L').tobytes()
    ref_ms += (time.perf_counter() - t) * 1e3
    ours = (out / f'{name}.y').read_bytes()
    if len(ours) != len(ref) or im.size != (w, h):
        bad.append((name, f'尺寸不同 {im.size} vs {(w, h)}'))
        continue
    d = [abs(a - b) for a, b in zip(ours, ref)]
    m = max(d)
    worst = max(worst, m)
    total_px += len(d)
    over += sum(1 for x in d if x > TOL)
    if m > TOL:
        bad.append((name, f'最大差 {m}'))
print(f'图像 {len(rows)}，像素 {total_px}，最大差 {worst}（上限 {TOL}），超限像素 {over}')
print(f'Pillow 平均每张 {ref_ms / max(1, len(rows)):.1f} ms（含转灰度）')
for b in bad[:20]:
    print('不合格', *b)
sys.exit(1 if bad else 0)

"""G1：用 libtiff（经 Pillow）编出 CCITT 码流，给 pagesift 的 CCITT 解码做对照。

    python3 -I benchmarks/history/1.0/g1/make_ccitt.py <输出目录> <张数> [种子]

每张随机宽高的黑白图按四种方式各编一次：T.6（G4）、T.4 一维、T.4 二维、T.4 二维且 EOL 按字节对齐。
每种写出 <名字>.ccitt（TIFF 条带里的原码流）、<名字>.params（pdfimages -ccitt 的参数格式）、
<名字>.png（原图，参照）。行内既有长游程（覆盖 1792–2560 的扩展组合码）也有短游程，
相邻行由上一行小幅错位得到（覆盖各种垂直模式和通过模式）。
Pillow 把黑白图存成 BlackIsZero，码流里的“黑”游程是图中的白，所以用 compare_ccitt.py 对照时
结果应全部是“反色一致”。
"""
import io, random, sys
from pathlib import Path
from PIL import Image

out, count = Path(sys.argv[1]), int(sys.argv[2])
rng = random.Random(int(sys.argv[3]) if len(sys.argv) > 3 else 1)
out.mkdir(parents=True, exist_ok=True)

# (名字, Pillow 的压缩方式, T4Options, pdfimages 参数里的编码方式, 是否字节对齐)
# T4Options（TIFF 6.0 第 11 节）：位 0 = 二维编码，位 2 = EOL 前补填充位使其以字节边界结束
MODES = [('g4', 'group4', None, '-4', False), ('g3', 'group3', 0, '-1', False),
         ('g3mr', 'group3', 1, '-2', False), ('g3fill', 'group3', 5, '-2', True)]


def row_runs(w):
    """一行：从白开始交替的游程，长短混合。"""
    runs, pos = [], 0
    while pos < w:
        kind = rng.random()
        r = rng.randint(0, 8) if kind < 0.5 else rng.randint(0, 200) if kind < 0.85 else rng.randint(0, 3000)
        runs.append(min(r, w - pos))
        pos += r
    return runs


def image(w, h):
    rows = []
    for y in range(h):
        if rows and rng.random() < 0.6:
            # 上一行的每个变化点移 −3…3（偶尔更多），保持单调
            prev = rows[-1]
            row = prev[:]
            for x in range(1, w):
                if prev[x] != prev[x - 1] and rng.random() < 0.5:
                    d = rng.randint(-3, 3) if rng.random() < 0.9 else rng.randint(-20, 20)
                    lo, hi = sorted((x, min(max(x + d, 0), w)))
                    for k in range(lo, hi):
                        row[k] = prev[x - 1] if d > 0 else prev[x]
            rows.append(row)
            continue
        row, white = [], True
        for r in row_runs(w):
            row += [255 if white else 0] * r
            white = not white
        rows.append(row[:w])
    im = Image.new('L', (w, h))
    im.putdata([v for r in rows for v in r])
    return im.convert('1')


for n in range(count):
    w = rng.choice([rng.randint(1, 64), rng.randint(65, 1800), rng.randint(1800, 3000)])
    h = rng.randint(1, 40)
    im = image(w, h)
    for name, comp, t4, enc, align in MODES:
        b = io.BytesIO()
        info = {} if t4 is None else {292: t4}
        im.save(b, 'TIFF', compression=comp, tiffinfo=info)
        t = Image.open(io.BytesIO(b.getvalue()))
        assert t.tag_v2.get(266, 1) == 1, '只处理高位在前'
        offs, lens = t.tag_v2[273], t.tag_v2[279]
        data = b''.join(b.getvalue()[o:o + l] for o, l in zip(offs, lens))
        stem = f'r{n:04d}-{name}'
        (out / f'{stem}.ccitt').write_bytes(data)
        a = '-A' if align else '-P'
        # pdfimages 不写行数；这里补上 -H，解码端没有它时解到数据末尾
        (out / f'{stem}.params').write_text(f'{enc} {a} -X {w} -H {h} -B -M\n')
        im.save(out / f'{stem}.png')

"""README 最上端的“性能一览”：从第 2 版测速结果取数，只列 pagesift 领先的速度项（RULES 第六节），四种语言。

    python3 -I benchmarks/mix/v2/readme_perf.py <en|zh|ja|es>

每行：同一批输入、同样的路数下，pagesift 0.80.9（CLI 和常驻 serve 取快的那个）对最快的竞品；倍数 = 两者吞吐之比；
内存是那一行两边的单 worker 峰值。证据链接到 v2 报告。
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import report as R  # noqa: E402

LANG = sys.argv[1]
LINK = "benchmarks/mix/v2/results/report-2026-10-28.md" if LANG == "en" else "../benchmarks/mix/v2/results/report-2026-10-28.md"
OURS = {"a2": ["pagesift", "pagesift-serve"], "b1": ["pagesift-file", "pagesift-serve-file"], "b4": ["pagesift-file", "pagesift-serve-file"]}
RIVALS = {
    "a2": ["crw", "trafilatura", "readability", "resiliparse", "newspaper4k"],
    "b1": ["pdf_oxide", "pymupdf4llm", "kreuzberg", "mineru-flash", "mineru", "docling", "docling-gpu"],
    "b4": ["kreuzberg", "markitdown", "mineru-flash", "pandoc", "extractous", "docling"],
}
MODELS = ["mineru", "docling", "docling-gpu"]
T = {
    "en": dict(title="## Performance at a glance (0.80.9)",
               intro="Measured on 2026-10-28 with pagesift 0.80.9 on one machine (i7-10700 8C/16T, 15 GB, GTX 1070), same inputs, same worker count, one tool at a time on a quiet machine; default settings for every tool. Only the rows where PageSift is fastest are listed here; where it is behind, see [How PageSift compares](#how-pagesift-compares) and the [full report]({link}).",
               head="| Workload | Workers | PageSift (↑ items/s) | Fastest competitor (↑ items/s) | Speed-up | Peak MB per worker (↓, PageSift / competitor) |",
               sub={"a2": "Main-content extraction, 269 random pages", "b1": "PDF, 144 olmOCR-bench pages", "b4": "Office & e-books, 81 files"},
               model="PDF vs layout-model tools (MinerU, Docling), same 144 pages", serve=" (resident `serve`)"),
    "zh": dict(title="## 性能一览（0.80.9）",
               intro="2026-10-28 用 pagesift 0.80.9 在同一台机器（i7-10700 8 核 16 线程、15 GB、GTX 1070）上测：同一批输入、同样的路数、机器空闲时逐个工具跑，全部默认设置。这里只列 PageSift 最快的项；落后的项见下面的[和同类工具比](#和同类工具比)和[完整报告]({link})。",
               head="| 任务 | 路数 | PageSift（份/秒，越高越好） | 最快的竞品（份/秒，越高越好） | 倍数 | 单 worker 峰值内存 MB（越低越好，PageSift / 竞品） |",
               sub={"a2": "网页正文提取，269 个随机网页", "b1": "PDF，olmOCR-bench 144 页", "b4": "Office 与电子书，81 份"},
               model="PDF 对版面模型工具（MinerU、Docling），同样 144 页", serve="（常驻 `serve`）"),
    "ja": dict(title="## パフォーマンス概要（0.80.9）",
               intro="2026-10-28、pagesift 0.80.9 を同じマシン（i7-10700 8 コア 16 スレッド、15 GB、GTX 1070）で計測。同じ入力・同じワーカー数で、マシンが空いている状態で 1 ツールずつ実行し、すべて既定設定。ここには PageSift が最速の項目だけを載せています。劣る項目は下の[他ツールとの比較](#他ツールとの比較)と[完全なレポート]({link})を参照してください。",
               head="| 処理 | ワーカー数 | PageSift（件/秒、高いほど良い） | 最速の競合（件/秒、高いほど良い） | 倍率 | ワーカーあたりピークメモリ MB（低いほど良い、PageSift / 競合） |",
               sub={"a2": "本文抽出、ランダムな 269 ページ", "b1": "PDF、olmOCR-bench 144 ページ", "b4": "Office と電子書籍、81 ファイル"},
               model="PDF 対レイアウトモデル系ツール（MinerU、Docling）、同じ 144 ページ", serve="（常駐 `serve`）"),
    "es": dict(title="## Rendimiento de un vistazo (0.80.9)",
               intro="Medido el 2026-10-28 con pagesift 0.80.9 en una sola máquina (i7-10700 8C/16T, 15 GB, GTX 1070): mismas entradas, mismo número de workers, una herramienta a la vez con la máquina en reposo, opciones por defecto en todas. Aquí solo aparecen las filas en las que PageSift es el más rápido; donde va por detrás, consulte la comparación de abajo y el [informe completo]({link}).",
               head="| Tarea | Workers | PageSift (↑ elementos/s) | Competidor más rápido (↑ elementos/s) | Aceleración | MB pico por worker (↓, PageSift / competidor) |",
               sub={"a2": "Extracción del contenido principal, 269 páginas aleatorias", "b1": "PDF, 144 páginas de olmOCR-bench", "b4": "Office y libros electrónicos, 81 archivos"},
               model="PDF frente a herramientas con modelos de maquetación (MinerU, Docling), mismas 144 páginas", serve=" (`serve` residente)"),
}[LANG]


def best(sub, tools, w):
    c = [(R.speed(sub, t, w), t) for t in tools]
    c = [(s, t) for s, t in c if s and s["tput"]]
    return max(c, key=lambda x: x[0]["tput"]) if c else (None, None)


def row(label, sub, w, rivals):
    s, t = best(sub, OURS[sub], w)
    r, rt = best(sub, rivals, w)
    if not s or not r or s["tput"] <= r["tput"]:
        return None
    ours = f"**{s['tput']:.1f}**" + (T["serve"] if "serve" in t else "")
    k = s["tput"] / r["tput"]
    ratio = f"×{k:.2f}" if k < 1.1 else f"×{k:.1f}"  # 领先很小时多给一位，不写成“×1.0”
    return f"| {label} | {w} | {ours} | {R.name(rt)} {r['tput']:.1f} | **{ratio}** | {s['peak']:.0f} / {r['peak']:.0f} |"


print(T["title"] + "\n")
print(T["intro"].format(link=LINK) + "\n")
print(T["head"])
print("|" + "---|" * 6)
for sub in ("b1", "b4", "a2"):
    for w in (1, 16):
        line = row(T["sub"][sub], sub, w, [x for x in RIVALS[sub] if x not in MODELS] if sub == "b1" else RIVALS[sub])
        if line:
            print(line)
line = row(T["model"], "b1", 1, MODELS)
if line:
    print(line)
print()

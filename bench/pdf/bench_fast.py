"""olmOCR-bench 的评分入口，结果和官方 benchmark.py 完全一致，只是数学测试快得多。

官方实现里，一条数学测试没在页面上找到原样相同的公式时，会把页面上每个 `$…$` 都用浏览器里的
KaTeX 渲染一遍再逐字比较。页面上公式多时（pagesift 每页输出上百个），这要做几十万次渲染。

这里加一个不改变结果的预筛：比较要求参考公式渲染出来的每个字符都能在候选公式的渲染里找到，
而渲染出来的 ASCII 字母和数字一定原样出现在 LaTeX 源码里（命令名只会让源码多出字母），所以源码里
ASCII 字母数字凑不齐参考公式的候选不可能通过，直接当作渲染失败跳过（原实现对渲染失败的候选也是跳过）。

用法和 benchmark.py 相同：python bench_fast.py --dir <目录> --candidate <工具>
（在 olmocr-bench 目录下、PYTHONPATH 指向它时运行）
"""
import collections
import sys
import threading

from olmocr.bench import tests as T
from olmocr.bench import benchmark

_local = threading.local()
_render = T.render_equation


def _alnum(s):
    return collections.Counter(c for c in s if c.isascii() and c.isalnum())


def _render_filtered(equation, *args, **kwargs):
    need = getattr(_local, "need", None)
    if need is not None:
        have = _alnum(equation)
        if any(have[c] < n for c, n in need.items()):
            return None
    return _render(equation, *args, **kwargs)


_run = T.MathTest.run


def _run_filtered(self, content):
    need = _alnum("".join(s.text for s in self.reference_render.spans))
    _local.need = need
    try:
        return _run(self, content)
    finally:
        _local.need = None


T.render_equation = _render_filtered
T.MathTest.run = _run_filtered

if __name__ == "__main__":
    sys.exit(benchmark.main())

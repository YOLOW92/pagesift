"""把 readme_perf.py 生成的“性能一览”放到四个 README 的最上端（徽章和语言切换之后的第一节），可重复执行：
已有的 <!-- perf-glance --> … <!-- /perf-glance --> 整段替换。

    python3 -I benchmarks/mix/v2/update_readme.py
"""
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
FILES = {"en": REPO / "README.md", "zh": REPO / "docs/README.zh-CN.md", "ja": REPO / "docs/README.ja.md", "es": REPO / "docs/README.es.md"}

for lang, path in FILES.items():
    sec = subprocess.run([sys.executable, "-I", str(HERE / "readme_perf.py"), lang], capture_output=True, text=True, check=True).stdout.strip()
    block = f"<!-- perf-glance -->\n{sec}\n<!-- /perf-glance -->\n\n---\n\n"
    s = path.read_text(encoding="utf-8")
    s = re.sub(r"<!-- perf-glance -->.*?<!-- /perf-glance -->\n\n---\n\n", "", s, flags=re.S)
    i = s.index("</div>")
    j = s.index("\n---\n", i) + len("\n---\n")
    s = s[:j] + "\n" + block + s[j:].lstrip("\n")
    path.write_text(s, encoding="utf-8")
    print(f"{path.relative_to(REPO)}：已更新")

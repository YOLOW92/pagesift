# D1/D3/D6/D7 的 Python 竞品统一镜像。
# 逐个安装：单个包失败（老库不适配 Python 3.12）不应让整批不可用，
# 装完逐个 import 自检，装不上的记进报告而不是静默忽略。
FROM python:3.12-slim

ENV PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential libxml2-dev libxslt1-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    for pkg in \
        trafilatura readability-lxml goose3 boilerpy3 justext resiliparse \
        html2text markdownify \
        pymupdf pymupdf4llm pypdf pdfplumber pdfminer.six markitdown \
        chardet charset-normalizer cchardet \
        extruct rdflib \
    ; do \
        echo "=== installing $pkg"; \
        pip install --no-cache-dir "$pkg" || echo "INSTALL_FAILED $pkg"; \
    done

# 自检：把能 import 的与失败的都打印出来
RUN python - <<'PY'
import importlib, json
mods = {
 "trafilatura":"trafilatura","readability-lxml":"readability","goose3":"goose3",
 "boilerpy3":"boilerpy3","justext":"justext","resiliparse":"resiliparse",
 "html2text":"html2text","markdownify":"markdownify",
 "pymupdf4llm":"pymupdf4llm","pypdf":"pypdf","pdfplumber":"pdfplumber",
 "pdfminer.six":"pdfminer","markitdown":"markitdown",
 "chardet":"chardet","charset-normalizer":"charset_normalizer","cchardet":"cchardet",
 "extruct":"extruct","rdflib":"rdflib",
}
ok, bad = [], []
for name, mod in mods.items():
    try:
        importlib.import_module(mod); ok.append(name)
    except Exception as e:
        bad.append((name, type(e).__name__))
print("IMPORT_OK", json.dumps(ok))
print("IMPORT_FAILED", json.dumps(bad))
PY

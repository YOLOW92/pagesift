# D3 PDF 竞品镜像
FROM python:3.12-slim
ENV PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential libmupdf-dev \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    for pkg in pymupdf pymupdf4llm pypdf pdfplumber pdfminer.six markitdown; do \
        echo "=== $pkg"; pip install --no-cache-dir "$pkg" || echo "INSTALL_FAILED $pkg"; \
    done
RUN python - <<'PY'
import importlib
mods={"pymupdf4llm":"pymupdf4llm","pypdf":"pypdf","pdfplumber":"pdfplumber",
      "pdfminer.six":"pdfminer","markitdown":"markitdown"}
for n,m in mods.items():
    try: importlib.import_module(m); print("OK",n)
    except Exception as e: print("FAIL",n,type(e).__name__)
PY

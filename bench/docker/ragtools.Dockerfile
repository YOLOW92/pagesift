FROM python:3.12-slim
ENV PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1
RUN apt-get update && apt-get install -y --no-install-recommends build-essential && rm -rf /var/lib/apt/lists/*
RUN set -eux; for p in "semchunk" "langchain-text-splitters" "chonkie" "rank_bm25"; do \
      echo "=== $p"; pip install --no-cache-dir "$p" || echo "FAILED $p"; done
RUN printf '%s\n' "import importlib" "for m in ['semchunk','langchain_text_splitters','chonkie','rank_bm25']:" "    try: importlib.import_module(m); print('OK',m)" "    except Exception as e: print('FAIL',m,type(e).__name__)" > /tmp/c.py && python /tmp/c.py

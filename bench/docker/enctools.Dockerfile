FROM python:3.12-slim
ENV PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1
RUN apt-get update && apt-get install -y --no-install-recommends uchardet build-essential \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; for p in chardet charset-normalizer cchardet; do pip install --no-cache-dir "$p" || echo "FAILED $p"; done
RUN printf '%s\n' "import importlib" "for m in ['chardet','charset_normalizer','cchardet']:" "    try: importlib.import_module(m); print('OK',m)" "    except Exception as e: print('FAIL',m,type(e).__name__)" > /tmp/chk.py && python /tmp/chk.py

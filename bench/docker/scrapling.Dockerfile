FROM python:3.12-slim
ENV PYTHONUNBUFFERED=1 PIP_DISABLE_PIP_VERSION_CHECK=1
RUN apt-get update && apt-get install -y --no-install-recommends build-essential libxml2-dev libxslt1-dev \
    && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir scrapling 2>&1 | tail -5 \
    && python -c "from scrapling.parser import Adaptor; print('scrapling OK')"

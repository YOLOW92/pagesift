# D9 浏览器渲染：Chrome + Playwright(Python) + puppeteer-core(Node)
#
# 关键点：PageSift 拒绝以 root 运行 Chrome（沙箱在 root 下无效，它坚持不关沙箱）。
# 因此镜像内必须让 chrome_sandbox 具备 setuid root 位，并以非 root 用户运行。
FROM mcr.microsoft.com/playwright/python:v1.49.0-noble
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates python3-pip \
    && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# 把 Playwright 的 chromium 目录搬到 PageSift 会探测的位置（mv 避免 cp -a 在层内失败）
RUN mkdir -p /opt/google \
    && mv /ms-playwright/chromium-1148/chrome-linux /opt/google/chrome \
    && ln -sf /opt/google/chrome/chrome /usr/bin/google-chrome

# 沙箱辅助程序必须是 root 所有且有 setuid 位，否则非 root 用户无法启动 Chrome
RUN chown root:root /opt/google/chrome/chrome_sandbox \
    && chmod 4755 /opt/google/chrome/chrome_sandbox \
    && chmod -R a+rX /opt/google/chrome \
    && chmod 755 /opt/google/chrome/chrome \
    && ls -la /opt/google/chrome/chrome_sandbox

RUN pip3 install --no-cache-dir --break-system-packages playwright==1.49.0 2>&1 | tail -1

ENV PUPPETEER_SKIP_DOWNLOAD=1
ENV PUPPETEER_EXECUTABLE_PATH=/opt/google/chrome/chrome
RUN mkdir -p /tools && cd /tools && npm init -y >/dev/null 2>&1 \
    && npm install --no-audit --no-fund puppeteer-core 2>&1 | tail -1

CMD ["/bin/bash"]

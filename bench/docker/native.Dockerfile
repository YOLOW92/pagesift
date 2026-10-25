# 原生二进制（PageSift 0.50/0.70、Rust 竞品适配器、Go 适配器）的统一运行基座。
# 只有 libc 等运行时依赖，所有二进制通过 -v 挂载进来，镜像本身不含被测代码。
FROM ubuntu:26.04
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

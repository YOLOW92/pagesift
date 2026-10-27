# pagesift 服务镜像：Firecrawl v2 兼容的 HTTP API，自带 Lightpanda 做浏览器渲染。
#
#   docker build -t pagesift --build-arg JOBS=$(scripts/jobs.sh) .
#   docker run -d -p 3002:3002 -v pagesift-data:/data -e PAGESIFT_API_KEYS=change-me pagesift
#
# 数据（缓存、任务、配额记录）在 /data，挂卷后重启容器任务接着跑。配置文件挂到
# /etc/pagesift/pagesift.toml 并设 PAGESIFT_CONFIG，或者直接用环境变量和命令行参数。
# 需要 Chrome 时用 [browser] remote 连外部浏览器（镜像里不带 Chrome）。

ARG RUST_VERSION=1.99
ARG LIGHTPANDA_VERSION=1.0.1

FROM rust:${RUST_VERSION}-bookworm AS build
ARG JOBS=4
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    CARGO_BUILD_JOBS=${JOBS} cargo build --release --locked -p pagesift \
    && cp target/release/pagesift /pagesift

FROM debian:bookworm-slim
ARG LIGHTPANDA_VERSION
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin pagesift \
    && mkdir -p /data /etc/pagesift \
    && chown pagesift:pagesift /data
# Lightpanda 固定版本并校验哈希；升级时一起改版本号和哈希
ADD --chmod=755 \
    --checksum=sha256:551747e35c51ba107b5d5277eca4b48e077b2a3b40f09a047bf6670448bc35fd \
    https://github.com/lightpanda-io/browser/releases/download/${LIGHTPANDA_VERSION}/lightpanda-x86_64-linux \
    /usr/local/bin/lightpanda
COPY --from=build /pagesift /usr/local/bin/pagesift
# 许可证和第三方署名随镜像走
COPY LICENSE NOTICE COMMERCIAL.md /usr/share/doc/pagesift/
ENV PAGESIFT_DATA_DIR=/data \
    PAGESIFT_LOG=info \
    PAGESIFT_LOG_FORMAT=json \
    LIGHTPANDA_DISABLE_TELEMETRY=true
USER pagesift
VOLUME /data
EXPOSE 3002
# tini 做 PID 1：转发 SIGTERM、回收浏览器子进程
ENTRYPOINT ["/usr/bin/tini", "--", "pagesift"]
CMD ["serve", "--host", "0.0.0.0", "--port", "3002"]

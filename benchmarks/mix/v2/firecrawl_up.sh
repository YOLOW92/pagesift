#!/usr/bin/env bash
# Firecrawl 自托管（A3 用）：照官方 docker-compose.yaml（firecrawl/firecrawl@90fe962e，SHA-256 561a7751…）的配置，
# 用 docker run 起 5 个服务（本机 docker 没有 compose 插件）。FoundationDB 只在设了 NUQ_BACKEND 时用，默认配置不起。
# 镜像用官方预构建的（compose 文件注释里给的写法），摘要记在 $MIX/firecrawl/images.lock。API 只暴露在 127.0.0.1:3002。
#   benchmarks/mix/v2/firecrawl_up.sh up|down
set -euo pipefail
NET=firecrawl-backend
case ${1:-up} in
down)
    docker rm -f fc-api fc-playwright fc-redis fc-rabbitmq fc-postgres >/dev/null 2>&1 || true
    docker network rm $NET >/dev/null 2>&1 || true
    docker volume rm fc-rabbitmq-data >/dev/null 2>&1 || true
    exit 0 ;;
esac
docker network create $NET >/dev/null 2>&1 || true
log=(--log-driver json-file --log-opt max-size=10m --log-opt max-file=3)
docker run -d --name fc-redis --network $NET --network-alias redis "${log[@]}" redis:alpine redis-server --bind 0.0.0.0 >/dev/null
docker run -d --name fc-rabbitmq --network $NET --network-alias rabbitmq "${log[@]}" -v fc-rabbitmq-data:/var/lib/rabbitmq rabbitmq:3-management rabbitmq-server >/dev/null
docker run -d --name fc-postgres --network $NET --network-alias nuq-postgres "${log[@]}" \
    -e POSTGRES_USER=postgres -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=postgres ghcr.io/firecrawl/nuq-postgres:latest >/dev/null
docker run -d --name fc-playwright --network $NET --network-alias playwright-service "${log[@]}" \
    -e PORT=3000 -e MAX_CONCURRENT_PAGES=10 --security-opt no-new-privileges:true --cap-drop ALL \
    --cpus 2.0 --memory 4g --memory-swap 4g --tmpfs /tmp/.cache:noexec,nosuid,size=1g \
    ghcr.io/firecrawl/playwright-service:latest >/dev/null
# 等 rabbitmq、postgres 就绪（compose 里 api 依赖它们的 healthcheck）
for _ in $(seq 1 60); do
    docker exec fc-rabbitmq rabbitmq-diagnostics -q check_running >/dev/null 2>&1 \
        && docker exec fc-postgres pg_isready -U postgres -d postgres >/dev/null 2>&1 && break
    sleep 2
done
docker run -d --name fc-api --network $NET --network-alias api "${log[@]}" --ulimit nofile=65535:65535 \
    --add-host host.docker.internal:host-gateway -p 127.0.0.1:3002:3002 --cpus 4.0 --memory 8g --memory-swap 8g \
    -e REDIS_URL=redis://redis:6379 -e REDIS_RATE_LIMIT_URL=redis://redis:6379 \
    -e PLAYWRIGHT_MICROSERVICE_URL=http://playwright-service:3000/scrape \
    -e POSTGRES_USER=postgres -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=postgres -e POSTGRES_HOST=nuq-postgres -e POSTGRES_PORT=5432 \
    -e USE_DB_AUTHENTICATION=false -e NUM_WORKERS_PER_QUEUE=8 -e CRAWL_CONCURRENT_REQUESTS=10 -e MAX_CONCURRENT_JOBS=5 -e BROWSER_POOL_SIZE=5 \
    -e HOST=0.0.0.0 -e PORT=3002 -e EXTRACT_WORKER_PORT=3004 -e WORKER_PORT=3005 -e NUQ_RABBITMQ_URL=amqp://rabbitmq:5672 \
    -e HARNESS_STARTUP_TIMEOUT_MS=60000 -e ENV=local \
    ghcr.io/firecrawl/firecrawl:latest node dist/src/harness.js --start-docker >/dev/null
for _ in $(seq 1 90); do
    curl -s -o /dev/null -w '%{http_code}' -X POST http://127.0.0.1:3002/v2/scrape -H 'Content-Type: application/json' \
        -d '{"url":"https://example.com","formats":["markdown"]}' 2>/dev/null | grep -q 200 && { echo "Firecrawl 就绪"; exit 0; }
    sleep 2
done
echo "Firecrawl 没起来，看 docker logs fc-api"; exit 1

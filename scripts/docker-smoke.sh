#!/usr/bin/env bash
# Docker 镜像的冒烟测试：起容器，检查健康检查、HTTP 抓取、Lightpanda 渲染、监控指标，
# 爬到一半重启容器（SIGTERM），确认任务从数据卷恢复并跑完。用到真实站点（每个一两次请求，遵守 robots.txt）。
#
#   docker build -t pagesift:dev --build-arg JOBS=$(scripts/jobs.sh) .
#   scripts/docker-smoke.sh pagesift:dev
set -euo pipefail

IMAGE=${1:-pagesift:dev}
NAME=pagesift-smoke-$$
VOLUME=$NAME-data
KEY=smoke-key
fail() { echo "FAIL $*" >&2; docker logs --tail 30 "$NAME" >&2 || true; exit 1; }
ok() { echo "ok   $*"; }
cleanup() {
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker volume rm "$VOLUME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker run -d --name "$NAME" -p 127.0.0.1::3002 -v "$VOLUME":/data \
    -e PAGESIFT_API_KEYS=$KEY "$IMAGE" >/dev/null
PORT=$(docker port "$NAME" 3002/tcp | head -1 | cut -d: -f2)
URL=http://127.0.0.1:$PORT

wait_healthy() {
    for _ in $(seq 100); do
        curl -fs "$URL/healthz" >/dev/null 2>&1 && return 0
        sleep 0.2
    done
    fail "server did not come up"
}
api() { curl -s -H "Authorization: Bearer $KEY" -H 'content-type: application/json' "$@"; }

wait_healthy
ok "healthz on port $PORT"
[ "$(docker exec "$NAME" id -u)" = 10001 ] && ok "runs as non-root" || fail "runs as root"

body=$(api -X POST "$URL/v2/scrape" -d '{"url":"https://example.com","formats":["markdown"],"engine":"http"}')
jq -e '.success and .data.metadata.title == "Example Domain" and (.data.markdown | test("documentation examples"))' <<<"$body" >/dev/null \
    && ok "http scrape" || fail "http scrape: $body"

body=$(api -X POST "$URL/v2/scrape" -d '{"url":"https://example.com","formats":["markdown"],"engine":"browser","maxAge":0}')
jq -e '.success and .data.metadata.pagesift.browserEngine == "lightpanda" and (.data.markdown | test("documentation examples"))' <<<"$body" >/dev/null \
    && ok "browser scrape with lightpanda" || fail "browser scrape: $body"

code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$URL/v2/scrape" -d '{"url":"https://example.com"}' -H 'content-type: application/json')
[ "$code" = 401 ] && ok "API key enforced" || fail "expected 401, got $code"

body=$(api -X POST "$URL/v2/crawl" -d '{"url":"https://example.com","webhook":"http://169.254.169.254/x"}')
jq -e '.code == "SSRF_BLOCKED"' <<<"$body" >/dev/null && ok "webhook SSRF refused" || fail "webhook: $body"

# 爬到一半重启容器：任务落在数据卷里，重启后接着跑
LIMIT=6
body=$(api -X POST "$URL/v2/crawl" -d "{\"url\":\"https://doc.rust-lang.org/book/\",\"limit\":$LIMIT,\"sitemap\":\"skip\",\"scrapeOptions\":{\"engine\":\"http\"}}")
ID=$(jq -r .id <<<"$body")
[ "$ID" != null ] || fail "crawl start: $body"
for _ in $(seq 300); do
    done_before=$(api "$URL/v2/crawl/$ID" | jq .completed)
    [ "$done_before" -ge 2 ] && break
    sleep 0.1
done
start=$(date +%s.%N)
docker restart -t 20 "$NAME" >/dev/null
stopped=$(echo "$(date +%s.%N) - $start" | bc)
PORT=$(docker port "$NAME" 3002/tcp | head -1 | cut -d: -f2)
URL=http://127.0.0.1:$PORT
wait_healthy
ok "restarted mid-crawl after $done_before pages (restart took ${stopped}s)"
status=""
for _ in $(seq 600); do
    status=$(api "$URL/v2/crawl/$ID")
    [ "$(jq -r .status <<<"$status")" = completed ] && break
    sleep 0.2
done
jq -e ".status == \"completed\" and .completed == $LIMIT and (.data | length) == $LIMIT and ([.data[].metadata.sourceURL] | unique | length) == $LIMIT" <<<"$status" >/dev/null \
    && ok "crawl resumed and completed with $LIMIT unique pages" \
    || fail "crawl after restart: $(jq -c '{status, completed, total, n: (.data|length)}' <<<"$status")"
docker logs "$NAME" 2>&1 | grep -q '"message":"resuming job"' && ok "resume logged" || fail "no resume log line"

m=$(curl -s "$URL/metrics")
grep -q 'pagesift_http_request_duration_seconds_bucket{route="/v2/crawl/{id}"' <<<"$m" \
    && grep -q 'pagesift_pages_scraped_total{source="crawl"}' <<<"$m" \
    && ok "metrics" || fail "metrics: $m"

start=$(date +%s.%N)
docker stop -t 20 "$NAME" >/dev/null
took=$(echo "$(date +%s.%N) - $start" | bc)
(( $(echo "$took < 5" | bc) )) && ok "SIGTERM stops the server in ${took}s" || fail "stop took ${took}s"
echo "all docker smoke checks passed"

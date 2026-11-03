# Jina Reader compatibility (`/r/`, `/s/`)

`GET /r/<url>` (and `POST /r` with `{"url": ...}`) reads one page; `GET /s/<query>` searches and reads
the top results. Both sit behind the same API keys, rate limits and quotas as the Firecrawl routes.
Request headers follow the [jina-ai/reader README](https://github.com/jina-ai/reader#using-request-headers);
a header pagesift recognizes but cannot honour is answered with `400 UNSUPPORTED_OPTION`, never
silently ignored. Unknown `x-` headers are ignored (proxies add their own).

| Header | pagesift |
|---|---|
| `x-respond-with` | `markdown` (whole page, no main-content detection), `html` (the fetched HTML), `text`, `frontmatter` (YAML `title`/`description`/`url` + main content), `markdown+frontmatter` (whole page), `screenshot` / `pageshot` (a `data:` URL; needs browser actions); `vlm`, `readerlm-v2`, `content` → unsupported. Without the header: main-content Markdown, as before |
| `x-engine` | `browser`, `curl` / `direct` (HTTP), `auto` |
| `x-target-selector` / `x-remove-selector` | `includeTags` / `excludeTags` (CSS selectors; a target selector turns off main-content detection) |
| `x-wait-for-selector` | a `wait` action before capture (needs browser actions) |
| `x-timeout` | seconds → `timeout`; ≥ 20 implies `network-idle` |
| `x-respond-timing` | `html` (no scripts, HTTP), `visible-content` (DOMContentLoaded), `mutation-idle` (DOM mutations settle: quiet for ln(1/δ) × the mean gap between the mutations observed on that page, δ = 0.05), `resource-idle` (scripts, XHR/fetch and documents quiet; the browser default), `media-idle` / `network-idle` (the `load` event, then requests quiet). Also available as the `respondTiming` scrape option |
| `x-max-tokens` | truncates at a token boundary; warning `output_truncated: <max> of <n> tokens` |
| `x-token-budget` | over budget → `413 TOO_LARGE` with the count (not applied on `/s/`) |
| `x-retain-images` | `all`, `none`, `alt` |
| `x-retain-links` | `all`, `none`, `text`; `gpt-oss` → unsupported |
| `x-retain-media` | `link`, `none`, `text`, `image` (video/audio by file extension or YouTube/Vimeo/Bilibili address); `html` → unsupported |
| `x-with-links-summary` / `x-with-images-summary` | `Links/Buttons:` / `Images:` footer, deduplicated; `all` keeps duplicates |
| `x-markdown-chunking` | `true` / `h1`…`h5` split at headings; `structured` / `s1`…`s5`: sections, paragraphs, lines, sentences, clauses. JSON responses carry `data.chunks`; text responses separate chunks with `---` |
| `x-preset` | `reader`, `index`, `research`, `agent`, `spider` (the bundles in Jina's cookbooks.md); explicit headers win |
| `x-no-cache` / `x-cache-tolerance` | `maxAge: 0` / `maxAge: <seconds>` |
| `x-set-cookie` | sent as the request `Cookie`; such requests are not cached |
| `x-proxy-url` | only the name of an `[[egress]]` configured in `pagesift.toml`; proxy URLs are refused so a request cannot be routed into an internal network |
| `x-with-generated-alt`, `x-detach-invisibles` | unsupported |
| `Accept: application/json` | `{"code": 200, "status": 20000, "data": {title, description, url, content, chunks?, warnings?}}` |
| `Accept: text/event-stream` | progress / content / done events |

Warnings of a text response are in the `x-pagesift-warnings` response header.

`/s/<query>` takes the top 5 results of the configured search backend (`[search]`), as Jina does,
reads each with the same headers and returns `[n] Title / URL Source / Description / Markdown Content`
blocks, or a JSON array with `Accept: application/json`. `?site=a.com&site=b.com` restricts the search
to those sites. A result that fails is reported in its own block; the others are still returned.

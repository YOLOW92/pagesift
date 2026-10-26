# 上传文件解析：`POST /v2/parse`

调用方直接上传文件的字节（不必先放到能访问的网址上），返回和 `POST /v2/scrape` 相同的文档。
写法兼容 Firecrawl 现行的 `/v2/parse`
（[接口文档](https://docs.firecrawl.dev/api-reference/endpoint/parse)，
源码 `apps/api/src/controllers/v2/parse.ts`）。MCP 不提供这个接口：文件不经 MCP 传。

支持的文件：**PDF**（主要场景）和 **HTML**（`.html`、`.htm`、`.xhtml`）。Word、Excel、PowerPoint
等 Firecrawl 支持的其他格式这里不支持，会明确报错，不会猜。

## 请求

`multipart/form-data`，两个字段：

| 字段 | 必填 | 内容 |
| :--- | :--- | :--- |
| `file` | 是 | 文件本身，上限 **50 MB**。文件名作为结果的 `metadata.sourceURL`（只保留最后一段路径）。 |
| `options` | 否 | JSON 对象的字符串，字段同 scrape 的选项，见下表。 |

```bash
curl -s http://localhost:3002/v2/parse \
  -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf \
  -F 'options={"formats":["markdown"],"parsers":[{"type":"pdf","maxPages":50,"pages":true}]};type=application/json'
```

文件类型的判断顺序：内容以 `%PDF-` 开头就是 PDF；否则看扩展名（`.pdf`、`.html`、`.htm`、`.xhtml`）；
再看这个字段的 Content-Type（`application/pdf`、`text/html`、`application/xhtml+xml`）。

### options

| 字段 | 默认 | 说明 |
| :--- | :--- | :--- |
| `formats` | `["markdown"]` | `markdown`、`html`、`rawHtml`、`links`、`images`；配了 LLM 时还有 `summary`、`json`。`screenshot`、`branding`、`changeTracking` 不支持。 |
| `onlyMainContent` | `true` | 对 HTML 生效：只保留正文。 |
| `includeTags` / `excludeTags` | 无 | 同 scrape。 |
| `timeout` | `30000` | 毫秒，1000–300000。超时回 408 `TIMEOUT`。 |
| `parsers` | `[{"type":"pdf"}]` | 也接受 `["pdf"]`。PDF 选项见下表。 |
| `queryFilter`、`chunking` | 无 | pagesift 扩展，同 scrape。 |
| `proxy` | 无 | 只接受 `basic`、`auto`（不联网，无实际作用）。 |
| `headers`、`skipTlsVerification`、`removeBase64Images`、`blockAds`、`origin`、`integration`、`zeroDataRetention` 等 | | 照收，不影响结果。 |

不能用的选项回 400 `UNSUPPORTED_OPTION`：`actions`、`waitFor`、`location`、`mobile`、
`engine: "browser"`、`egress`、`discoverScripts`、条件请求、`basic`/`auto` 以外的 `proxy`。

PDF 选项（`parsers` 里的 `{"type": "pdf", ...}`）：

| 字段 | 说明 |
| :--- | :--- |
| `mode` | `fast`、`auto`（默认）：都读 PDF 的文字层。`ocr` 不支持（回 `UNSUPPORTED_OPTION`）；扫描件没有文字层时有 `scanned PDF` 警告。 |
| `maxPages` | 1–10000。只输出前这么多页，并给 `max_pages` 警告；注意整份文件仍会解析一遍。 |
| `pages` | `true` 时另给每页的 Markdown：`data.pages: [{pageNumber, markdown}]`。 |
| `blocks`、`pageMarkers` | 不支持，设为 `true` 回 `UNSUPPORTED_OPTION`。 |

## 响应

和 `/v2/scrape` 相同：`{"success": true, "data": {markdown, ..., metadata}}`。

- PDF：`markdown` 与用 `/v2/scrape` 抓同一份文件逐字相同；`metadata.title`、`metadata.numPages` 也相同。
- HTML：正文提取和 scrape 相同。没有 `<base href>` 时相对链接按 `file:///<文件名>` 解析，不冒充任何网址；
  `links` 只收 http(s) 链接，所以这些相对链接不出现在 `links` 里。
  上传的页面看起来像拦截页时照样提取，只给 `access_barrier` 警告（抓取时这是错误）。
- `metadata.sourceURL` 是文件名，`statusCode` 是 200，`contentType` 是 `application/pdf` 或上传时的 HTML 类型，
  `metadata.pagesift.engine` 是 `upload`。

## 错误

响应体和其他接口一样是 `{"success": false, "code", "error"}`：

| 情况 | 状态码 | `code` |
| :--- | :--- | :--- |
| 不是 multipart、multipart 格式坏了、缺 `file`、文件为空、`options` 不是 JSON 对象、选项取值不对 | 400 | `INVALID_REQUEST`（Firecrawl 是 `BAD_REQUEST`） |
| 文件类型不支持、加密 PDF、做不到的选项 | 400 | `UNSUPPORTED_OPTION`（Firecrawl 对类型是 `UNSUPPORTED_FILE_TYPE`） |
| PDF 损坏读不了 | 400 | `INVALID_REQUEST`，`error` 以 `could not read PDF` 开头 |
| 文件超过 50 MB（声明的 Content-Length 超限时不读请求体）；PDF 超过 5000 页 | 413 | `TOO_LARGE` |
| 请求体 120 秒内没传完；解析超过 `timeout` | 408 | `TIMEOUT` |
| API key 不对；超过速率、并发或配额 | 401 / 429 | 同其他接口 |

## 资源

- 内存：文件只在内存里保留一份。按 Content-Length 一次分配好缓冲区，边收边追加，超限立刻停；
  之后这块缓冲区原样交给解析器，解析完就释放。50 MB 的文件峰值约为 50 MB 加解析本身的用量。
- CPU：和抓取共用解析阶段的闸门与按核数限制的 CPU 名额，大量上传不会把机器压垮。
- 请求体时限：普通请求 30 秒，上传单独放宽到 120 秒（`Timeouts::upload_body`）；整请求时限相应多加 90 秒，
  传得慢也不会挤占解析的 `timeout`。
- 审计日志：每次解析记一条 `kind: "parse"`，`url` 为 `upload:<文件名>`，另有 `fileBytes`；不存快照，
  上传的文件往往不公开。
- 监控：`pagesift_pages_scraped_total{source="parse"}`，以及按路由 `/v2/parse` 的请求数和延迟。

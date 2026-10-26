# 上传文件解析：`POST /v2/parse`（以及 scrape 抓到的文档）

调用方直接上传文件的字节（不必先放到能访问的网址上），返回和 `POST /v2/scrape` 相同的文档。
写法兼容 Firecrawl 现行的 `/v2/parse`
（[接口文档](https://docs.firecrawl.dev/api-reference/endpoint/parse)，
源码 `apps/api/src/controllers/v2/parse.ts`）。MCP 不提供这个接口：文件不经 MCP 传。

支持的文件：

| 格式 | 扩展名 | 转换 |
| :--- | :--- | :--- |
| PDF | `.pdf` | pagesift-pdf（文字层） |
| HTML | `.html`、`.htm`、`.xhtml` | 和网页相同的正文提取 |
| Word | `.docx`（含 docm/dotx） | pagesift-office：标题、列表、表格、图片替代文字、链接、脚注、公式（LaTeX） |
| Excel | `.xlsx`（含 xlsm/xltx） | 每个工作表一个二级标题和一张表，单元格是按数字格式显示的值 |
| PowerPoint | `.pptx`（含 pptm/ppsx） | 每张幻灯片 `<!-- Slide N -->`、标题、要点、表格、图片、图表数据、演讲者备注 |
| EPUB | `.epub` | 按 spine 顺序，每章交给和网页相同的 HTML 提取器（保留整章） |
| CSV | `.csv`、`.tsv` | 一张表（RFC 4180，编码和分隔符自动识别） |

旧版二进制格式（`.doc`、`.xls`、`.ppt`）、OpenDocument（`.odt` 等）、RTF 不支持，明确报错，不会猜。
`/v2/scrape` 抓到这些格式时走同一段转换代码（见文末）；`pagesift scrape <本地文件>` 也一样。

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

文件类型的判断顺序：内容以 `%PDF-` 开头就是 PDF；ZIP 容器按里面的部件认 Word（`word/document.xml`）、
Excel（`xl/workbook.xml`）、PowerPoint（`ppt/presentation.xml`）、EPUB（`META-INF/container.xml`），
文件名和类型写错也认得出；加了口令的 Office 文件（OLE 复合文件）按扩展名认，转换时报“已加密”；
其余看扩展名，再看这个字段的 Content-Type。CSV 没有签名，只能靠扩展名或 `text/csv`。

### options

| 字段 | 默认 | 说明 |
| :--- | :--- | :--- |
| `formats` | `["markdown"]` | `markdown`、`html`、`rawHtml`、`links`、`images`；配了 LLM 时还有 `summary`、`json`。`screenshot`、`branding`、`changeTracking` 不支持。PDF 和 Office 文档只有 `markdown`（另见 `parsers: []`）。 |
| `onlyMainContent` | `true` | 对 HTML 生效：只保留正文。 |
| `includeTags` / `excludeTags` | 无 | 同 scrape。 |
| `timeout` | `30000` | 毫秒，1000–300000。超时回 408 `TIMEOUT`。 |
| `parsers` | `["pdf", "image"]` | PDF 选项见下表。 |
| `queryFilter`、`chunking` | 无 | pagesift 扩展，同 scrape。 |
| `proxy` | 无 | 只接受 `basic`、`auto`（不联网，无实际作用）。 |
| `headers`、`skipTlsVerification`、`removeBase64Images`、`blockAds`、`origin`、`integration`、`zeroDataRetention` 等 | | 照收，不影响结果。 |

不能用的选项回 400 `UNSUPPORTED_OPTION`：`actions`、`waitFor`、`location`、`mobile`、
`engine: "browser"`、`egress`、`discoverScripts`、条件请求、`basic`/`auto` 以外的 `proxy`。

### `parsers`（parse 和 scrape 相同）

照 Firecrawl：每项是解析器名字（`"pdf"`、`"image"`）或对象（`{"type": "pdf", ...}`）。不给时等于默认值；
给了列表时，列表里没有的解析器关闭。

| 写法 | 效果 |
| :--- | :--- |
| 不给、`["pdf"]`、`["pdf", "image"]` | 正常解析 PDF。`"image"`（图片 OCR）照收：pagesift 本来就不处理图片，图片网址照旧回 `UNSUPPORTED_OPTION`。 |
| `[]`、`["image"]` | PDF 不解析，和 Firecrawl 一样把原文件按 base64 放进 `markdown`（要了 `html` 时也放进 `html`）。 |
| `{"type": "pdf", "mode": "fast" \| "auto"}` | 都读文字层。`fast` 遇到扫描件（没有文字层）回 400 `UNSUPPORTED_OPTION`（Firecrawl 是 `SCRAPE_PDF_OCR_REQUIRED`）；`auto` 照常返回并给 `scanned PDF` 警告。`ocr` 不支持（回 `UNSUPPORTED_OPTION`）。 |
| `{"type": "pdf", "maxPages": N}` | 1–10000。**只解析前 N 页**，后面的页不解释、不排版；给 `max_pages` 警告。`metadata.numPages` 是转换的页数，`metadata.totalPages` 是文件的页数（和 Firecrawl 相同）。 |
| `{"type": "pdf", "pages": true}` | 另给每页的 Markdown：`data.pages: [{pageNumber, markdown}]`（scrape 也给）。 |
| `blocks`、`pageMarkers` | 不支持，设为 `true` 回 `UNSUPPORTED_OPTION`。 |

写错的 `parsers`（非数组、未知项、取值越界）在 scrape 里也是抓取之前就回 400。`parsers` 参与缓存键
（不给时缓存键和以前一样）。

## 响应

和 `/v2/scrape` 相同：`{"success": true, "data": {markdown, ..., metadata}}`。

- PDF：`markdown` 与用 `/v2/scrape` 抓同一份文件逐字相同；`metadata.title`、`numPages`、`totalPages` 也相同。
- HTML：正文提取和 scrape 相同。没有 `<base href>` 时相对链接按 `file:///<文件名>` 解析，不冒充任何网址；
  `links` 只收 http(s) 链接，所以这些相对链接不出现在 `links` 里。
  上传的页面看起来像拦截页时照样提取，只给 `access_barrier` 警告（抓取时这是错误）。
- Office、EPUB、CSV：`metadata.title` 取文档属性（docProps/core.xml、OPF 的 dc:title）；截断等提示进
  `metadata.pagesift.warnings`；耗时记在 `metadata.pagesift.timings.<扩展名>`。
- `metadata.sourceURL` 是文件名，`statusCode` 是 200，`contentType` 是格式对应的 MIME（HTML 是上传时的类型），
  `metadata.pagesift.engine` 是 `upload`。

## 错误

响应体和其他接口一样是 `{"success": false, "code", "error"}`。**scrape 抓到的 PDF、Office 文档出错时状态码与此相同**
（以前 scrape 一律回 502）：

| 情况 | 状态码 | `code` |
| :--- | :--- | :--- |
| 不是 multipart、multipart 格式坏了、缺 `file`、文件为空、`options` 不是 JSON 对象、选项取值不对 | 400 | `INVALID_REQUEST`（Firecrawl 是 `BAD_REQUEST`） |
| 文件类型不支持、加密的 PDF/Office/EPUB（DRM）、做不到的选项 | 400 | `UNSUPPORTED_OPTION`（Firecrawl 对类型是 `UNSUPPORTED_FILE_TYPE`） |
| 文件损坏读不了 | 400 | `INVALID_REQUEST`，`error` 以 `could not read PDF`（或 `DOCX`、`XLSX`……）开头 |
| 文件超过 50 MB（声明的 Content-Length 超限时不读请求体）；PDF 超过 5000 页；解压后超限（见下） | 413 | `TOO_LARGE` |
| 请求体没在时限内传完（默认 120 秒，可配）；解析超过 `timeout` | 408 | `TIMEOUT` |
| API key 不对；超过速率、并发或配额 | 401 / 429 | 同其他接口 |

## 资源与安全

- 内存：文件只在内存里保留一份。按 Content-Length 一次分配好缓冲区，边收边追加，超限立刻停；
  之后这块缓冲区原样交给解析器，解析完就释放。
- Office/EPUB 的上限（`pagesift_office::Limits`）：ZIP 条目 2 万个；单个条目解压后 256 MB、整个文件累计 512 MB；
  4 MB 以上的条目压缩比不超过 400:1（防 ZIP 炸弹）；XML 嵌套 512 层；表格（Excel、CSV）共 200 万个单元格、
  1024 列，超出的部分截掉并给警告。XML 不展开任何 DOCTYPE 里声明的实体，没有外部实体（XXE）和实体膨胀的问题。
- CPU：和抓取共用解析阶段的闸门与按核数限制的 CPU 名额，大量上传不会把机器压垮。
- 时限：服务端的四个时限都能在 `pagesift.toml` 的 `[server.timeouts]` 里配（秒，必须大于 0）：
  `header_read_secs`（读请求头，默认 30）、`body_secs`（读请求体，默认 30）、
  `upload_body_secs`（`/v2/parse` 的请求体，默认 120）、`request_secs`（整个请求，默认 360）。
  上传放宽的请求体时间会加到整请求时限上，传得慢也不会挤占解析的 `timeout`。
- 审计日志：每次解析记一条 `kind: "parse"`，`url` 为 `upload:<文件名>`，另有 `fileBytes`；不存快照，
  上传的文件往往不公开。
- 监控：`pagesift_pages_scraped_total{source="parse"}`，以及按路由 `/v2/parse` 的请求数和延迟。

## `/v2/scrape` 抓到文档时

抓到的内容按这个顺序判断：PDF（`%PDF-` 开头或 `application/pdf`）→ Office/EPUB（ZIP 里的部件，或 Content-Type，
或网址路径的扩展名）→ CSV（`text/csv`，或网址以 `.csv`/`.tsv` 结尾且服务器没说是 HTML）→ 其他按网页处理。
转换和上传完全相同，`metadata.pagesift.engine` 是 `http`。

## 和 Firecrawl 有意不同的地方

| 项目 | Firecrawl | pagesift | 理由 |
| :--- | :--- | :--- | :--- |
| 错误码名 | `BAD_REQUEST` / `UNSUPPORTED_FILE_TYPE` / `PARSE_UNSUPPORTED_OPTIONS` | `INVALID_REQUEST` / `UNSUPPORTED_OPTION` | 和现有 scrape 的错误码一致；状态码都是 400 |
| 超限 | 文档未写 | 413 `TOO_LARGE` | 沿用现有的 `TOO_LARGE` |
| 坏 PDF / 坏文档 | 500 | 400 `INVALID_REQUEST` | 是文件的问题，不是服务端故障 |
| 加密 PDF / Office / DRM 的 EPUB | — | 400 `UNSUPPORTED_OPTION` | 需要口令或密钥，做不到 |
| 类型判断 | 先扩展名，再 MIME | 先看内容（`%PDF-`、ZIP 里的部件），再扩展名、MIME | 文件名写错也能认出 |
| `mode: ocr`、`blocks`、`pageMarkers` | 支持 | 明确报不支持 | 不悄悄降级 |
| `mode: fast` 遇到扫描件 | `SCRAPE_PDF_OCR_REQUIRED` | 400 `UNSUPPORTED_OPTION` | 同样报错，错误码用现有的 |
| `image` 解析器（图片 OCR） | 支持 | 照收，不做 OCR | 不处理图片；收下是为了 Firecrawl SDK 的默认值不被拒 |
| 文档的 `html` 格式 | Markdown 转出的 HTML | 只给 `markdown` | 不为文档另造 HTML |
| 支持的文档格式 | 另有 doc、xls、ppt、odt/ods/odp、rtf | docx、xlsx、pptx、epub、csv | 本轮范围；旧二进制格式和 OpenDocument 留待以后 |
| Word 页眉页脚 | 放在正文前后 | 不输出 | 每页重复的内容会混进正文 |
| Excel 隐藏的行列 | 未核实（Firecrawl 用 anydoc 转换；MarkItDown 照样输出） | 不输出 | 用户在表格软件里看不到 |

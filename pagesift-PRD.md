# pagesift 产品需求文档（PRD）

| 项 | 内容 |
|---|---|
| 版本 | 1.1 草案（评审后修订） |
| 日期 | 2026-10-21 |
| 作者 | YOLOW92（产品负责人），Claude（起草） |
| 状态 | 已评审，按此开发 |

---

## 1. 一句话定位

**pagesift：一个用 Rust 写的单文件网页抓取引擎。正文提取和拦截页识别是自研的，接口兼容 Firecrawl v2，把网页变成干净的 Markdown 和结构化数据，供大模型和智能体使用；采用 MIT/Apache-2.0 许可，守规矩，不绕过反爬。**

```bash
curl -fsSL https://pagesift.dev/install.sh | sh
pagesift serve        # 在 :3002 提供 Firecrawl 兼容接口，把 SDK 的 base URL 改过来就能用
```

---

## 2. 背景与机会

### 2.1 市场

| 项目 | 星数（2026-10） | 语言 | 许可证 | 情况 |
|---|---|---|---|---|
| firecrawl/firecrawl | 193k | TypeScript | **AGPL-3.0** | 自部署的 docker-compose 默认起 7 个服务（api、playwright、redis、rabbitmq、postgres、foundationdb 等）；自部署版的内置引擎不支持截图和 actions，要接付费的 Fire-engine 才有 |
| unclecode/crawl4ai | 86k | Python | Apache-2.0 | 依赖 Playwright 和 Python 环境，资源占用高 |
| fastcrw/crw | 1.2k | Rust | **AGPL-3.0**（另售商业授权，贡献需签 CLA） | 兼容接口主要挂在 `/firecrawl/v2` 下，根路径 `/v2` 是已弃用的别名；可选的 TLS 指纹伪装（编译时开启）和 stealth 方案 |
| spider-rs/spider | 2.8k | Rust | MIT | 只是爬虫库，不兼容 Firecrawl 接口，没有面向大模型的输出 |

**空位**：目前没有一个成熟项目同时满足"宽松许可证 + 兼容 Firecrawl + 单文件 + 守规矩"。（已知的宽松许可证兼容实现都是个位数星的小项目。）

### 2.2 需求来源

需求最初来自我们自己的证据图谱产品 AiSeek。它在抓取上遇到的问题在同类用户中很普遍：

- 因为 AGPL 和部署太重而拒绝了 Firecrawl；也拒绝所有浏览器伪装和反爬绕过方案。
- 浏览器进程失控，留下大量僵尸进程，内存占用很高；
- 普通抓取慢；
- 验证码页和拦截页被当成正文存下来；
- 被拒、被限流、超时这几种情况分不清。

**AiSeek 的专属集成不在 1.0 范围内**。AiSeek 以后直接调用标准的 `/v2/scrape` 接口。

---

## 3. 目标用户与场景

| 用户 | 场景 | 核心诉求 |
|---|---|---|
| RAG 和知识库开发者 | 把文档站、博客整站抓下来，转成 Markdown 后切块入库 | 正文干净，能整站爬取，增量更新 |
| 智能体开发者 | 通过 MCP 让智能体读网页、提取内容 | 速度快，能直接给 MCP 调用 |
| 自托管 Firecrawl 的用户 | 想摆脱 AGPL 和臃肿的部署 | 改一行 base URL 就能迁移，资源占用小 |
| 监测和存证类产品 | 定期抓取固定页面，发现变化并留证 | 状态如实返回、有内容哈希、支持条件请求、能识别拦截页 |
| Rust 开发者 | 在自己的程序里直接调用 | 能当库用，不需要单独起一个服务 |

---

## 4. 产品原则

1. **兼容优先**：官方 Firecrawl SDK（Python/JS）、LangChain、LlamaIndex、Dify、n8n 不改代码就能接入；唯一的例外是绕过类参数（见 6.5.1）。
2. **守规矩**：默认遵守 robots.txt 和 Crawl-delay，按主机限速。**不提供**任何反爬绕过：不伪装 TLS 指纹，不提供隐身模式，不破解验证码，不轮换代理去规避封禁。被拒就如实报告，绝不重试硬闯。
3. **状态如实**：明确区分成功、被 robots 禁止、被拦截页挡住、被限流、超时、目标出错，每种情况都有固定的错误码。请求了做不到的事就明确报错，不悄悄降级。
4. **单文件部署**：一个可执行文件，不需要 Redis、数据库或消息队列。浏览器渲染（1.x）会用到系统里的 Chrome/Chromium。
5. **核心自研，基础设施用成熟的开源项目**：决定产品质量和差异化的部分自己做；协议解析、网络、密码学这类通用基础设施用业界标准实现（见第 9 节）。
6. **宽松许可证**：MIT OR Apache-2.0，可以商用、嵌入、修改，没有传染性；所有依赖也必须是宽松许可证。

---

## 5. 竞品对比（1.0 目标状态）

数字一律注明来源；竞品的数字以我们在本地自己实测的为准，发布前更新。

| 能力 | Firecrawl 自部署 | crw | Crawl4AI | **pagesift 1.0** |
|---|---|---|---|---|
| 许可证 | AGPL | AGPL | Apache | **MIT/Apache** |
| 部署 | 默认 7 个服务 | 单文件 | Python 加 Playwright | **单文件** |
| 空闲内存 | 数 GB（待实测） | 待实测 | 待实测 | **目标 < 20MB（存储为空时）** |
| 官方 SDK 零修改接入 | ✅ | 部分（JS SDK 要加 `/firecrawl` 后缀） | ❌ | **✅** |
| 截图和页面操作 | 要接付费 Fire-engine | 部分支持 | ✅ | 1.x |
| 拦截页识别与如实状态 | 待核实 | 待核实 | 待核实 | **✅** |
| 内容哈希、条件请求 | 待核实 | 待核实 | 待核实 | **✅** |
| 内置 MCP | 单独项目 | ✅ | ❌ | **✅** |
| 能当库嵌入 | ❌ | AGPL 限制 | Python | **Rust ✅；Python/Node 计划中** |
| 反爬绕过 | 付费版有 | 可选开启 | 部分 | **❌（刻意不做）** |

---

## 6. 功能需求

优先级：**P0** 进 1.0；**1.x** 发布后按版本逐步补。
发布线是"核心可用"：第 6 节所有 P0 完成，官方 SDK 端到端测试通过，就发布 1.0。

### 6.1 抓取引擎

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-1.1 | HTTP 客户端（基于 hyper/reqwest）：连接池、keep-alive、gzip/deflate/br/zstd 解压、超时（连接、首字节、总时长）；HTTP/2 跟着库一起支持 | P0 |
| F-1.2 | 重定向：最多 N 跳（默认 5），**每一跳都重新做 SSRF 校验和 robots 校验**，并记录重定向链 | P0 |
| F-1.3 | **SSRF 防护（自研）**：自定义 DNS 解析层，拒绝内网、回环、链路本地、元数据地址和运营商 NAT 等地址；DNS 结果里只要有一个非公网地址就整体拒绝，防止 DNS 重绑定；拦截 IP 字面量和非 http(s) 协议 | P0 |
| F-1.4 | 响应大小上限（默认 15MB），超过就截断并标记 | P0 |
| F-1.5 | 字符集识别：依次看 HTTP 头、BOM、`<meta charset>`、启发式判断（用 `encoding_rs`） | P0 |
| F-1.6 | 条件请求：支持 `If-None-Match` 和 `If-Modified-Since`；遇到 304 返回 `notModified: true`，不重新解析 | P0 |
| F-1.7 | 默认 User-Agent 是 `pagesift/<版本> (+https://pagesift.dev/bot)`，可以配置，但**不允许冒充浏览器的 UA 预设** | P0 |
| F-1.8 | `engine: http|browser|auto`。1.0 只有 HTTP 引擎：`auto` 等于 `http`；`browser` 返回 `UNSUPPORTED_OPTION` | P0 |
| F-1.9 | 浏览器渲染引擎：正文太少或命中 SPA 特征时由 `auto` 升级到浏览器；找不到浏览器时退回 HTTP 结果，并附带 `pagesift.warnings: ["render_unavailable"]` | 1.x |
| F-1.10 | 代理：支持标准 HTTP/SOCKS5 出口代理，用于企业网络环境；不做轮换代理池 | 1.x |

### 6.2 礼貌爬取（自研）

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-2.1 | robots.txt 解析器，遵循 RFC 9309：通配符、`$`、最长匹配优先、Allow 和 Disallow、Crawl-delay、Sitemap；按主机缓存 24 小时；拿到 5xx 时按"全部禁止"处理，404 按"全部允许"处理 | P0 |
| F-2.2 | 按主机限速：默认每秒 1 个请求，Crawl-delay 优先；全局并发上限和单主机并发上限都可配置 | P0 |
| F-2.3 | 自适应退避：遇到 429 或 503 时遵守 `Retry-After`；连续失败时指数退避，主机进入冷却期（可配置） | P0 |
| F-2.4 | **robots 规则**：单页 `/v2/scrape` 默认遵守 robots，调用方可以在请求里用 `ignoreRobotsTxt: true` 关闭（这是和 Firecrawl 的行为差异，Firecrawl 的单页 scrape 默认不检查）。`/v2/crawl` 和 `/v2/map` 上的 `ignoreRobotsTxt` 必须由管理员在配置里开启后才生效，否则返回 `UNSUPPORTED_OPTION` | P0 |

### 6.3 内容处理

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-3.1 | HTML 解析和 CSS 选择器用 html5ever 体系，用于 DOM 构建和 `includeTags`、`excludeTags` | P0 |
| F-3.2 | **自研正文提取**：思路参考 Readability 的打分算法，再加上自己的改进：中文文本密度、对列表页和文档站的识别、保留代码块和表格、去掉导航、页脚、Cookie 横幅、广告和"相关推荐"；`onlyMainContent` 默认开启 | P0 |
| F-3.3 | **自研 Markdown 生成器**：生成 GFM（标题、列表、表格、代码块带语言、引用、链接和图片转成绝对 URL、脚注），合并多余空行，去掉零宽字符 | P0 |
| F-3.4 | 元数据提取：title、description、language、keywords、robots、og 和 twitter 系列、Dublin Core、canonical、favicon；作者和发布、修改时间依次从 JSON-LD、meta、`<time>`、正文启发式里找，统一换算成 UTC | P0 |
| F-3.5 | 输出格式：`markdown`、`html`（清洗后的正文）、`rawHtml`、`links`、`images`。其余格式（`summary`、`screenshot`、`json`、`changeTracking`、`attributes`）在 1.0 返回 `UNSUPPORTED_OPTION` | P0 |
| F-3.6 | **自研拦截页识别**：带特征库，覆盖 Cloudflare、Akamai、DataDome、PerimeterX、Imperva、阿里云、腾讯云 WAF、reCAPTCHA、hCaptcha 和常见登录墙；用状态码加正文长度一起判断，避免误伤。命中时返回 `ACCESS_BARRIER`、厂商名、置信度和命中的规则 ID，**不返回正文**，也不提供强制返回正文的开关 | P0 |
| F-3.7 | 内容指纹：返回原始 HTML、正文 Markdown 和规范化纯文本三者的 sha256；纯文本的哈希先去掉空白差异，用于判断内容是否变化 | P0 |
| F-3.8 | 纯文本输出，以及按 token 估算的长度（方便调用方分块） | 1.x |
| F-3.9 | PDF 文本提取，输出 Markdown；扫描件不做 OCR，只标记出来 | 1.x |
| F-3.10 | `summary` 和 `json` 格式（需要大模型，兼容 OpenAI 接口的服务商） | 1.x |

### 6.4 浏览器渲染（1.x，自研 CDP 客户端）

整个模块放在 1.x。需求先记录在这里，CDP 客户端可以提前开发，但不在 1.0 的关键路径上。

| 编号 | 需求 |
|---|---|
| F-4.1 | 自研 CDP 客户端（WebSocket 层可以用现成的库），驱动系统里的 Chrome、Chromium 或兼容 CDP 的浏览器（如 obscura、moli），通过配置指定 |
| F-4.2 | **有上限的浏览器池**：常驻进程数和每个进程的标签页数都有上限；每处理 N 个页面回收一次进程；子进程放进同一个进程组，退出时整组清理，**保证不留僵尸进程**；有健康检查，浏览器崩溃后自动拉起 |
| F-4.3 | 子请求拦截：通过 CDP Fetch 域，让每个子请求都过 SSRF 检查，可选屏蔽广告、字体、媒体资源 |
| F-4.4 | `waitFor`、`timeout`、`mobile`（模拟视口和触屏，**不改 UA 去伪装**）、`location.languages` 映射到 Accept-Language |
| F-4.5 | `actions`：wait、click、write、press、scroll、screenshot、scrape、executeJavascript、pdf；`executeJavascript` 只有在配置里开启后才可用 |
| F-4.6 | 截图：支持视口截图、整页截图和质量参数，以 base64 或缓存 URL 返回 |
| F-4.7 | 浏览器以非 root 身份运行，不加沙箱逃逸参数 |

### 6.5 接口（API）

#### 6.5.1 Firecrawl v2 兼容（挂在根路径 `/v2/*`）

| 接口 | 说明 | 优先级 |
|---|---|---|
| `POST /v2/scrape` | 单页抓取 | P0 |
| `POST /v2/crawl`、`GET /v2/crawl/:id`、`DELETE /v2/crawl/:id`、`GET /v2/crawl/:id/errors`、`GET /v2/crawl/active` | 整站爬取：异步任务，查询状态时返回绝对地址的 `next` 用于分页 | P0 |
| `POST /v2/map` | 站点 URL 清单：合并 sitemap、robots 里声明的 Sitemap 和首页链接，支持 `search` 过滤 | P0 |
| `POST /v2/batch/scrape`、`GET`、`DELETE`、`/errors` | 批量抓取，复用 crawl 的任务系统 | P0 |
| `WS /v2/crawl/:id` | 实时推送爬取进度 | 1.x |
| `POST /v2/search` | 搜索加抓取，搜索后端可插拔（SearXNG、Brave、Bing、Google CSE、Tavily 等），用户自己提供 key | 1.x |
| `POST /v2/extract`、`GET /v2/extract/:id` | 用大模型做结构化提取 | 1.x |
| `POST /v2/crawl/params-preview`、`POST /v2/parse` | 预览爬取参数、上传文件解析 | 1.x |
| `POST /v1/scrape`、`/v1/map`、`/v1/crawl` | 照顾老客户端的 v1 响应格式 | 1.x |

兼容细节：
- 同时接受 `Authorization: Bearer` 和不带 key 的请求（由配置决定）。
- 请求里的未知字段要忽略，不能报错。
- 响应字段名和类型与 Firecrawl 完全一致；`metadata` 里附加的字段统一放在 `pagesift` 命名空间下，防止和原字段冲突。
- **错误响应**：响应体沿用 Firecrawl 的格式 `{success: false, code, error}`，`code` 里填 pagesift 的错误码。HTTP 状态码向 Firecrawl 的同类错误看齐，保证官方 SDK 抛出的异常类型不变（见 6.5.2）。
- **绕过类参数**：`proxy` 取 `auto` 或 `basic` 时照常放行；`stealth`、`enhanced` 和其他取值，以及未经授权的 `ignoreRobotsTxt`（见 F-2.4），都返回 `UNSUPPORTED_OPTION`（400），并在 `error` 里说明 pagesift 不提供这项能力。
- **未实现的功能**：1.0 还没实现的参数（如 `actions`、`screenshot` 格式）同样返回 `UNSUPPORTED_OPTION`，不悄悄忽略。
- 迁移指南里列出和 Firecrawl 的全部行为差异，第一条是"单页 scrape 默认遵守 robots.txt"。

#### 6.5.2 pagesift 原生扩展

| 内容 | 说明 | 优先级 |
|---|---|---|
| 扩展请求参数 | `engine`、`ifNoneMatch`、`ifModifiedSince` | P0 |
| 扩展响应字段 | `pagesift.contentSha256`、`pagesift.rawSha256`、`pagesift.textSha256`、`pagesift.redirectChain`、`pagesift.etag`、`pagesift.lastModified`、`pagesift.notModified`、`pagesift.barrier`（厂商、置信度、规则 ID）、`pagesift.timings`、`pagesift.warnings` | P0 |
| 统一错误码 | 见下表 | P0 |
| `GET /healthz`、`GET /metrics` | 健康检查，以及 Prometheus 文本格式的监控指标 | P0 |
| `storeSnapshot` 参数和 `POST /v2/diff` | 快照，以及同一 URL 两个快照之间的块级差异对比，区分价格、日期、计数这类"噪声变化"；拦截页的原始 HTML 同样会存进快照并标记 | 1.x |

错误码和 HTTP 状态码（状态码向 Firecrawl 的同类错误看齐，开发时以 Firecrawl 源码为准逐一核对）：

| code | HTTP | 说明 |
|---|---|---|
| `ROBOTS_DISALLOWED` | 403 | 被 robots.txt 禁止 |
| `ACCESS_BARRIER` | 403 | 被拦截页挡住，附带厂商、置信度、规则 ID |
| `RATE_LIMITED` | 429 | 被目标站点限流，附带 Retry-After |
| `HOST_COOLDOWN` | 503 | 主机处于冷却期 |
| `TIMEOUT` | 408 | 超时，和 Firecrawl 的 `SCRAPE_TIMEOUT` 对齐 |
| `SSRF_BLOCKED` | 400 | 目标地址不允许访问 |
| `TOO_LARGE` | 413 | 响应超过大小上限 |
| `UPSTREAM_ERROR` | 502 | 目标站点出错或网络错误 |
| `UNSUPPORTED_OPTION` | 400 | 绕过类参数，或 1.0 尚未实现的功能 |
| `INVALID_REQUEST` | 400 | 请求格式错误 |

### 6.6 整站爬取与站点清单

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-6.1 | 广度优先爬取，支持这些参数：`limit`、`maxDiscoveryDepth`、`includePaths`、`excludePaths`（正则，用 `regex` crate）、`allowSubdomains`、`allowExternalLinks`、`crawlEntireDomain`、`ignoreQueryParameters`、`sitemap: include|skip|only`、`delay`、`maxConcurrency` | P0 |
| F-6.2 | URL 规范化：协议和主机转小写、去掉默认端口、去掉 fragment、可选排序或去掉查询参数、处理 `..` 路径；用来去重 | P0 |
| F-6.3 | sitemap 解析：sitemapindex、urlset、gzip 压缩的 sitemap、lastmod（XML 用现成的解析库） | P0 |
| F-6.4 | 任务持久化：进程重启后能接着爬，状态落在本地存储里（见 6.8） | 1.x |
| F-6.5 | Webhook：started、page、completed、failed 事件，附带 HMAC 签名 | 1.x |

### 6.7 MCP 服务器（自研协议实现）

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-7.1 | 支持 stdio 和 Streamable HTTP 两种传输；JSON-RPC 2.0 和 MCP 协议自己实现 | P0 |
| F-7.2 | 1.0 的工具：`pagesift_scrape`、`pagesift_map`、`pagesift_crawl`、`pagesift_crawl_status`、`pagesift_batch_scrape`。统一用 `pagesift_` 前缀，避免和其他 MCP 服务器重名；参数结构与 firecrawl-mcp 的对应工具保持一致 | P0 |
| F-7.3 | `pagesift_search`、`pagesift_extract`、`pagesift_diff`，跟随对应接口上线 | 1.x |
| F-7.4 | 一行命令完成配置：`pagesift mcp install --client claude|cursor|codex|...` | 1.x |

### 6.8 存储与缓存（自研）

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-8.1 | 内嵌存储：只追加的日志文件加索引，存缓存和任务状态；不依赖外部数据库。索引放内存还是放磁盘，做到这一步时根据实测再定（见第 13 节） | P0 |
| F-8.2 | `maxAge`、`minAge`、`storeInCache` 的语义与 Firecrawl 一致；缓存键是 URL 加影响结果的参数 | P0 |
| F-8.3 | 快照：原始 HTML 压缩后保存（可设置大小上限，默认 512KB），按内容哈希去重，可以按 URL 和时间查询 | 1.x |
| F-8.4 | 存储空间上限和 LRU 淘汰；提供 `pagesift gc` 命令 | 1.x |

### 6.9 命令行（CLI）

```
pagesift serve [--port 3002] [--config pagesift.toml]
pagesift scrape <url> [--format md|json|html] [--only-main]
pagesift crawl <url> [--limit 100] [--out dir/]   # 每个页面输出一个 .md 文件
pagesift map <url>
pagesift mcp [stdio|http]
pagesift doctor                                   # 检查网络、配置（1.x 加上浏览器检测）
pagesift bench <dataset>                          # 跑正文提取效果评测
```

P0：serve、scrape、crawl、map、mcp、doctor、bench。1.x：search、mcp install。

### 6.10 库和多语言绑定

| 编号 | 需求 | 优先级 |
|---|---|---|
| F-10.1 | Rust crate：`pagesift::scrape(url, opts).await` 这类异步接口 | P0 |
| F-10.2 | Python 绑定（PyPI 包 `pagesift`），用 PyO3 | 1.x |
| F-10.3 | Node 绑定（npm 包 `pagesift`），用 napi-rs | 1.x |

### 6.11 配置与安全

- 配置来源的优先级：命令行参数 > 环境变量（`PAGESIFT_*`）> `pagesift.toml`。
- 鉴权：可以配置多个 API key，每个 key 可以有自己的限速。**鉴权和计量做成可插拔的 trait**，给将来可能的托管服务留扩展点，1.0 不做多租户和计费。
- 主机白名单和黑名单；`allowPrivateNetwork` 默认关闭。
- 管理员开关：是否允许 crawl 和 map 忽略 robots（F-2.4）。

---

## 7. 非功能需求

| 类别 | 指标 |
|---|---|
| 平台 | 1.0：Linux x86_64 和 aarch64（musl 静态链接）、macOS x86_64 和 aarch64。Windows 放到 1.x |
| 体积 | 单个可执行文件，无运行时依赖，小于 15MB |
| 内存 | 存储为空时空闲内存小于 20MB；HTTP 模式、100 并发时小于 200MB。存储索引的内存开销做到存储时再定指标 |
| 启动 | 冷启动小于 50ms |
| 吞吐 | 单核每秒处理 ≥ 300 个页面（解析加转 Markdown，不算网络时间） |
| 延迟 | 本地解析加转 Markdown 的 p50 小于 5ms（100KB 的页面） |
| 正文质量 | 报两个口径，三家（pagesift、crw、Firecrawl 自部署）都在本地用同一套脚本重新跑：① **crw 口径的召回率**（Firecrawl 公开的 scrape-content-dataset-v1，819 个有标注的 URL，短语命中 ≥ 30% 算找到；crw 自称 63.7%，自部署 Firecrawl 56.0%），目标 ≥ 65%；② **自定义的严格口径**：token 级 F1，同时衡量正文召回和噪声去除，目标高于 crw 和 trafilatura |
| 可靠性 | 连续运行 72 小时、10 万个页面，内存不持续增长 |
| 兼容性 | 官方 Python 和 JS SDK、LangChain、LlamaIndex、Dify、n8n 在 1.0 已实现接口上的端到端测试全部通过 |
| 安全 | SSRF 测试集全部通过：重定向到内网、DNS 重绑定、IPv6 映射地址、十进制和八进制写法的 IP |
| 代码质量 | 自己的代码里不写 `unsafe`；对自研的解析器（robots、正文提取）做模糊测试；核心模块测试覆盖率 ≥ 80% |
| 依赖许可证 | 只允许宽松许可证（MIT、Apache-2.0、BSD、ISC、Zlib 等），在 CI 里用 cargo-deny 卡住，禁止 GPL、AGPL、SSPL 类依赖 |

---

## 8. 架构

```
                  ┌──────────────────────────── pagesift（单文件） ─────────────────────────────┐
  SDK / Dify /    │  api: Firecrawl v2 兼容 · 原生扩展 · /metrics                              │
  n8n / curl ───► │  mcp: stdio · Streamable HTTP（自研协议）                                  │
  智能体 (MCP) ──► │  cli                                                                       │
                  │ ─────────────────────────────────────────────────────────────────────────── │
                  │  jobs: crawl / batch 调度 · 持久化                                         │
                  │  policy: robots · 主机限速 · 退避冷却 · 白名单 · SSRF（自研）              │
                  │  fetch: HTTP 客户端（hyper/reqwest）· DNS 防护 · 字符集 · 条件请求        │
                  │  content: HTML 解析（html5ever）· 正文提取 · Markdown · 元数据 · 拦截页识别（自研） │
                  │  store: 只追加日志 · 缓存（自研）                                           │
                  │  render (1.x): CDP 客户端（自研）· 浏览器池 · 子请求拦截                    │
                  └───────────────────────────────────────────────────────────────────────────┘
```

Cargo workspace 里的 crate 划分：

| crate | 内容 |
|---|---|
| `pagesift-content` | 正文提取、Markdown 生成、元数据提取、拦截页识别（核心，可以单独发布） |
| `pagesift-net` | HTTP 客户端封装、DNS 防护、字符集处理 |
| `pagesift-policy` | robots.txt、限速、冷却 |
| `pagesift-store` | 存储、缓存 |
| `pagesift-api` | HTTP 服务器、路由、Firecrawl 兼容层 |
| `pagesift-mcp` | MCP 协议实现 |
| `pagesift-cdp` | CDP 客户端、浏览器池（1.x） |
| `pagesift` | 对外的库接口和 CLI |

`pagesift-content` 本身就可以是一个独立的"HTML 转干净 Markdown"库，发布时可以作为单独的亮点。

---

## 9. 依赖策略

**原则：决定产品质量和差异化的部分自研；通用基础设施用成熟的宽松许可证开源项目。**

| 类别 | 处理方式 |
|---|---|
| 正文提取、Markdown 生成、拦截页识别、元数据提取 | **自研**（产品核心） |
| robots.txt、限速、冷却、SSRF 防护和 DNS 校验层 | **自研**（"守规矩"和安全的核心） |
| Firecrawl 兼容层、错误码体系 | **自研** |
| MCP 和 JSON-RPC、CDP 客户端 | **自研** |
| 内嵌存储 | **自研** |
| 异步运行时 | tokio |
| HTTP 客户端和服务器 | hyper / reqwest / axum |
| TLS 和密码学、哈希 | rustls、webpki-roots；sha2 |
| HTML 解析和 CSS 选择器 | html5ever 体系 |
| 压缩、字符集 | flate2、brotli、zstd；encoding_rs |
| 正则、XML、TOML、JSON | regex、quick-xml、toml、serde/serde_json |
| CLI 参数、日志、指标 | clap、tracing 等 |

不再以"依赖数量少"为卖点。约束只有一条：**所有依赖必须是宽松许可证**，由 cargo-deny 在 CI 里强制检查。

---

## 10. 开发与发布方式

- **公开开发**：仓库从第一天起就是 public，但 1.0 发布前不主动宣传，把冲击力留给发布那一次。
- **项目归属**：放在个人账号 YOLOW92 下（`github.com/YOLOW92/pagesift`）。
- **工期不设死**，按里程碑推进；发布线是"核心可用"（第 6 节所有 P0）。
- 每个里程碑结束时跑一次正文质量评测，用数字跟踪进度。
- 建议尽早注册 `pagesift.dev`，并在 crates.io 上占住 `pagesift` 这个名字（2026-10-21 查过，域名、GitHub、crates.io、PyPI、npm 上的这个名字都还没人用）。

---

## 11. 里程碑（开发顺序）

| 里程碑 | 内容 |
|---|---|
| M1 基础设施 | workspace、CI（含 cargo-deny）、模糊测试框架、配置、CLI 框架、日志和指标 |
| M2 评测先行 | 接入 Firecrawl scrape-content-dataset-v1，写好两个口径的评测脚本；在本地跑出 crw 和自部署 Firecrawl 的基线 |
| M3 网络与策略 | HTTP 客户端封装、DNS 防护、SSRF 测试集、字符集、条件请求；robots、限速、冷却 |
| M4 内容处理 | 正文提取、Markdown 生成、元数据提取、拦截页识别；对照评测持续调优（最耗时的一步） |
| M5 接口 | `/v2/scrape`、`/v2/map`、错误码体系、兼容层、内容哈希 |
| M6 爬取与存储 | sitemap、整站爬取、批量抓取、内嵌存储、缓存 |
| M7 MCP | MCP 协议、stdio 和 Streamable HTTP、5 个工具 |
| M8 发布准备 | SDK 和集成端到端测试、性能与质量基准、迁移指南、文档、安装脚本、多平台构建 |

1.x 路线：浏览器渲染（CDP 客户端、浏览器池、actions、截图）→ 快照和 `/v2/diff` → search、extract → PDF → Python/Node 绑定 → Windows。

---

## 12. 发布与增长计划

1. **README 第一屏**：一行安装命令、一个 10 秒的动图、一张和 Firecrawl、crw、Crawl4AI 的对比表（许可证、内存、启动时间、两个口径的正文质量，全部注明来源），以及"改一行 base URL 就能迁移"的代码示例。
2. **可复现的基准测试**：`pagesift bench` 加上公开数据集，结果和测试脚本都放在仓库里。
3. **迁移指南**：从 Firecrawl 自部署迁移（含全部行为差异清单）、从 Crawl4AI 迁移、在 Dify 和 n8n 里接入，各写一篇。
4. **发布渠道**：Hacker News（Show HN）、Reddit（r/rust、r/LocalLLaMA、r/selfhosted）、V2EX、即刻、少数派、Rust 中文社区、X/Twitter；同时提交到 awesome-mcp-servers、awesome-rust、awesome-selfhosted。
5. **发布时间**：避开大厂发布会，选周二到周四、美国东部时间上午发 Show HN。
6. **生态**：向 Dify、n8n、LangChain 提 PR，加入"Firecrawl 兼容服务"的配置说明。
7. **1.x 持续曝光**：浏览器渲染、diff、`pagesift-content` 独立 crate，每个都可以单独发一次。

---

## 13. 风险

| 风险 | 影响 | 应对 |
|---|---|---|
| 正文提取质量达不到目标 | 核心卖点不成立 | M2 先把评测搭好，从第一天起用数字驱动调优 |
| 工期不定，一直拖着发不出去 | 市场窗口被抢 | 发布线定为"核心可用"；每个里程碑都跑评测，看得到进度 |
| crw 或其他项目先改成宽松许可证 | 差异化变弱 | 把"守规矩、状态如实、正文质量"这几个差异点做扎实 |
| Firecrawl 改接口 | 兼容性被破坏 | 每晚针对最新 SDK 跑端到端测试，兼容层做成独立模块 |
| "守规矩"导致部分网站抓不到 | 用户流失 | 当成功能来宣传：如实返回状态，提供清晰的错误码和文档；目标用户是合规场景 |
| 自研存储的内存索引随数据量增长 | 内存指标不成立 | 做到存储时实测，再决定索引放内存还是放磁盘 |
| 名字被抢注 | 品牌受损 | 尽早注册域名，并在 crates.io 上占位 |
| 被滥用于大规模抓取 | 声誉问题 | 默认限速并遵守 robots，crawl 忽略 robots 必须由管理员开启，UA 可以追溯，不提供任何绕过功能 |

---

## 14. 成功指标

| 时间 | 指标 |
|---|---|
| 发布当周 | GitHub ≥ 1,000 星，HN 首页 |
| 发布 3 个月 | ≥ 5,000 星；至少 1 个知名项目（Dify、n8n 等）的文档提到 pagesift |
| 发布 6 个月 | ≥ 10,000 星；crates.io 月下载 ≥ 50k |
| 质量 | crw 口径召回率 ≥ 65%；严格口径 F1 高于 crw 和 trafilatura；1.0 范围内兼容性测试 100% 通过 |

---

## 15. 评审决议记录（2026-10-21）

| 问题 | 决议 |
|---|---|
| 首要目标 | 技术作品，冲星数；AiSeek 集成移出 1.0 |
| 自研边界 | 正文提取、Markdown、拦截页识别、礼貌策略、SSRF、兼容层、MCP、CDP、存储自研；其余用宽松许可证的开源项目 |
| 依赖卖点 | 删掉"少于 10 个依赖"，改为"只用宽松许可证"，由 cargo-deny 强制 |
| 发布线 | 核心可用：scrape、map、crawl、batch、MCP、CLI、内容哈希、条件请求 |
| 工期 | 不设死，按里程碑推进 |
| 公开方式 | 仓库公开，1.0 前不主动宣传（最初定的是闭门开发，后来改了） |
| 项目归属 | 个人账号 YOLOW92 |
| 平台 | Linux + macOS；Windows 放到 1.x |
| 绕过类参数 | 报 `UNSUPPORTED_OPTION`；`proxy: auto/basic` 放行 |
| 拦截页 | 报错不返回正文，附带置信度和规则 ID；快照上线后照样留存 |
| robots | 单页 scrape 默认遵守、请求可关；crawl 和 map 忽略 robots 需要管理员开启 |
| 错误响应 | Firecrawl 格式，状态码向 Firecrawl 看齐 |
| MCP 命名 | `pagesift_` 前缀，参数与 firecrawl-mcp 对齐 |
| 质量口径 | crw 口径和严格 F1 两个都报，三家都在本地实测 |
| 存储 | 自研；内存索引的问题做到时再定 |
| 商业化 | 只留可插拔的鉴权和计量扩展点 |
| 语言绑定 | 1.x，用 PyO3 和 napi-rs |

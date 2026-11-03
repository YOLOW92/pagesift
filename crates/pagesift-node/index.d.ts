/* eslint-disable */
/* 由 napi-rs 根据 src/ 生成；本段手写的类型来自 header.d.ts。 */

/** 包版本。 */
export declare const version: string

/** 和 pagesift HTTP 接口一致的错误码。 */
export type ErrorCode =
  | 'ROBOTS_DISALLOWED'
  | 'ACCESS_BARRIER'
  | 'RATE_LIMITED'
  | 'HOST_COOLDOWN'
  | 'TIMEOUT'
  | 'SSRF_BLOCKED'
  | 'TOO_LARGE'
  | 'UPSTREAM_ERROR'
  | 'NETWORK_ERROR'
  | 'DNS_FAILURE'
  | 'BUSY'
  | 'UNSUPPORTED_OPTION'
  | 'INVALID_REQUEST'
  | 'INTERNAL_ERROR'

/** 抓取或转换失败时抛出（或 Promise 以它拒绝）。 */
export declare class PagesiftError extends Error {
  readonly name: 'PagesiftError'
  readonly code: ErrorCode
  /** 补充信息，比如拦截页的 `{ vendor, kind, confidence, ruleId }`。 */
  readonly details?: unknown
  /** 限流时建议多少秒后再试。 */
  readonly retryAfter?: number
}

export type Format = 'markdown' | 'html' | 'rawHtml' | 'links' | 'images'

/** `Page.toJSON()` 的结果：一次取出全部字段的普通对象，没请求的格式不出现。 */
export interface PageJSON {
  markdown?: string
  html?: string
  rawHtml?: string
  links?: string[]
  images?: string[]
  metadata: PageMetadata
}

export interface PageMetadata {
  title?: string
  description?: string
  language?: string
  keywords?: string
  robots?: string
  favicon?: string
  publishedTime?: string
  modifiedTime?: string
  /** 最终地址（跟随重定向之后）。 */
  url?: string
  /** 请求的地址。 */
  sourceURL?: string
  /** 只有抓取结果才有。 */
  statusCode?: number
  contentType?: string
  proxyUsed?: string
  /** PDF 的页数。 */
  numPages?: number
  /** 抓取结果的 pagesift 扩展字段。 */
  pagesift?: PagesiftMetadata
  /** `ogImage`、`twitterCard`、`articlePublishedTime` 这类 meta，键名是 camelCase。 */
  [key: string]: unknown
}

export interface PagesiftMetadata {
  /** Markdown、原始 HTML、正文纯文本的 BLAKE3 哈希（十六进制）。 */
  contentHash?: string
  /** Computed only when change tracking is enabled in a supporting API surface. */
  rawHash?: string
  textHash?: string
  redirectChain?: string[]
  etag?: string
  lastModified?: string
  /** 条件请求命中，页面没变（此时没有 markdown）。 */
  notModified?: boolean
  /** 这一页实际用的抓取方式。 */
  engine?: 'http' | 'browser'
  warnings?: string[]
  /** 各阶段耗时，毫秒。 */
  timings?: Record<string, number>
}


/**
 * Which binding artifact the generated loader actually loaded: `'native'` for
 * a native addon, otherwise the `platformArchABI` of the WASI flavor. Every
 * flavor napi-rs can build is listed, because `NAPI_RS_NATIVE_LIBRARY_PATH`
 * can point the loader at a WASI artifact this package does not build itself.
 */
export declare const __napiBindingTarget: 'native' | 'wasm32-wasi' | 'wasm32-wasip1'

declare global {
  interface __NapiRsAsyncGenerator<TOwner, T, TReturn, TNext> {
    next(...[value]: [] | [TNext]): globalThis.Promise<globalThis.IteratorResult<T, TReturn | undefined>>
    return(...[value]: [] | [TReturn]): globalThis.Promise<globalThis.IteratorResult<T, TReturn | undefined>>
    throw(exception?: unknown): globalThis.Promise<globalThis.IteratorResult<T, TReturn | undefined>>
    [globalThis.Symbol.asyncIterator](): this
  }
}

/**
 * 进行中的整站爬取，每完成一页产出一个 `CrawlItem`（`for await`）。提前 `break` 或 `cancel()` 停止爬取；
 * 结束后 `summary` 给出停止原因。爬取本身出错（比如路径正则写错）时迭代以 Error 拒绝，
 * 消息以错误码开头（如 `INVALID_REQUEST: …`）。
 *
 * This type implements JavaScript's async iterable protocol.
 * It can be used with `for await...of` loops.
 * 
 * @see https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Iteration_protocols#the_async_iterator_and_async_iterable_protocols
 */
export declare class CrawlStream {
  /** 停止爬取：引擎不再调度新页面，之后迭代立即结束，已完成、还没取出的页面也丢弃。 */
  cancel(): void
  /** 已经确定要抓的页面数（随着发现新链接增长，不超过 `limit`）。 */
  get total(): number
  /** 引擎已经交付的成功页数（含还没取出的）。 */
  get pages(): number
  /** 引擎已经交付的失败页数（含还没取出的）。 */
  get errors(): number
  /** 爬取结束后的说明；结束前为 null。 */
  get summary(): CrawlSummary | null

  [globalThis.Symbol.asyncIterator](): globalThis.__NapiRsAsyncGenerator<CrawlStream, CrawlItem, void, undefined>
}

/** 一页的转换或抓取结果。每次读属性都会新建一个 JS 值，需要多次使用时先存到变量里。 */
export declare class Page {
  /** 正文的 Markdown；`formats` 里没有 `markdown`、或条件请求命中（`notModified`）时为 null。 */
  get markdown(): string | null
  /** 清洗后的 HTML（`formats` 含 `html` 时才有）。 */
  get html(): string | null
  /** 原始 HTML（`formats` 含 `rawHtml` 时才有）。 */
  get rawHtml(): string | null
  /** 页面里的链接，绝对地址（`formats` 含 `links` 时才有）。 */
  get links(): Array<string> | null
  /** 页面里的图片地址（`formats` 含 `images` 时才有）。 */
  get images(): Array<string> | null
  /** 标题、描述、状态码、`og:*` 等元数据，以及 `pagesift` 扩展字段。 */
  get metadata(): PageMetadata
  /** 一次性取出全部字段，得到普通对象（`JSON.stringify(page)` 也会调用它）。 */
  toJSON(): PageJSON
}

/** 抓取器：连接池、robots.txt 缓存、限速状态和浏览器池都在它里面，长期复用一个实例最快。 */
export declare class Scraper {
  constructor(config?: ScraperConfig | undefined | null)
  /** 抓取一个 URL。失败时 Promise 以 `PagesiftError` 拒绝。 */
  scrape(url: string, options?: ScrapeOptions | undefined | null): Promise<Page>
  /** 抓取一批 URL，全部完成后一次返回，顺序和输入一致；单个失败只影响它自己的那一项。 */
  scrapeMany(urls: Array<string>, options?: BatchOptions | undefined | null): Promise<BatchItem[]>
  /** 抓取一批 URL，按完成顺序逐个产出（`for await`）。提前 `break` 会取消还没完成的抓取。 */
  scrapeStream(urls: Array<string>, options?: BatchOptions | undefined | null): ScrapeStream
  /** 整站爬取，每完成一页产出一项（`for await`）；提前 `break` 或 `cancel()` 停止爬取。 */
  crawl(url: string, options?: CrawlOptions | undefined | null): CrawlStream
  /** 站点的链接列表（sitemap 加页面上的链接）。失败时 Promise 以 `PagesiftError` 拒绝。 */
  map(url: string, options?: MapOptions | undefined | null): Promise<MapLink[]>
  /**
   * 解析一个文件（PDF、HTML、Word、Excel、PowerPoint、EPUB、CSV），结果和 `scrape` 一样。
   * 类型按内容识别，认不出时再看文件名的扩展名和 `contentType`。失败时 Promise 以 `PagesiftError` 拒绝。
   */
  parse(data: Uint8Array, filename: string, options?: ParseOptions | undefined | null): Promise<Page>
  /** 解析一批文件，按完成顺序逐个产出（`for await`），`url` 是文件名；并发和取消语义同 `scrapeStream`。 */
  parseStream(files: Array<ParseFile>, options?: BatchOptions | undefined | null): ScrapeStream
  /** 关闭浏览器进程。之后再调用抓取方法会报错。 */
  close(): Promise<void>
}

/**
 * 按完成顺序产出 `BatchItem` 的异步迭代器（`scrapeStream`、`parseStream`）。
 *
 * This type implements JavaScript's async iterable protocol.
 * It can be used with `for await...of` loops.
 *
 * @see https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Iteration_protocols#the_async_iterator_and_async_iterable_protocols
 */
export declare class ScrapeStream {

  [globalThis.Symbol.asyncIterator](): globalThis.__NapiRsAsyncGenerator<ScrapeStream, BatchItem, void, undefined>
}

/** 批量结果里单个 URL 的错误：字段和 `PagesiftError` 一致，但不是 Error 实例（不采集调用栈）。 */
export interface BatchError {
  code: ErrorCode
  message: string
  details?: unknown
  /** 限流时建议多少秒后再试。 */
  retryAfter?: number
}

/** 批量抓取（或批量解析）里一项的结果：`page` 和 `error` 恰好有一个。 */
export interface BatchItem {
  /** 在输入数组里的下标。 */
  index: number
  /** 抓取时是 URL，解析时是文件名。 */
  url: string
  page?: Page
  error?: BatchError
}

/** 批量抓取选项：`ScrapeOptions` 的全部字段，加上并发数。 */
export interface BatchOptions {
  /** 同时进行的抓取数，默认 16；同一主机仍受限速约束。 */
  concurrency?: number
  /** 计算哪些输出，默认 `['markdown']`；只算需要的，越少越快。 */
  formats?: Format[]
  /** 只保留正文，去掉导航、页眉页脚、侧边栏，默认 true。 */
  onlyMainContent?: boolean
  /** `auto`（默认，正文太少时改用浏览器渲染）、`http` 或 `browser`。 */
  engine?: 'auto' | 'http' | 'browser'
  /** 总超时，毫秒，至少 1000。 */
  timeout?: number
  /** 浏览器渲染时页面加载后再等待多久，毫秒。 */
  waitFor?: number
  /** 额外的请求头。 */
  headers?: Record<string, string>
  /** 这一页不检查 robots.txt（只对单页抓取有效）。 */
  ignoreRobotsTxt?: boolean
  /** 浏览器渲染时模拟手机。 */
  mobile?: boolean
  /** 浏览器渲染时屏蔽广告，默认 true。 */
  blockAds?: boolean
  /** 条件请求：`If-None-Match`。 */
  ifNoneMatch?: string
  /** 条件请求：`If-Modified-Since`（HTTP 日期）。 */
  ifModifiedSince?: string
  /** 出口代理（http://, https://, socks5://）。 */
  proxy?: string
}

/** 把一段 HTML 转成 Markdown，在当前线程上同步执行（会阻塞事件循环，适合脚本和小页面）。 */
export declare function convert(html: string | Uint8Array, options?: ConvertOptions): Page

/** 和 `convert` 相同，但在后台线程上执行，不阻塞事件循环。 */
export declare function convertAsync(html: string | Uint8Array, options?: ConvertOptions): Promise<Page>

/**
 * 批量转换：使用共享的 Rust CPU 名额，结果顺序和输入一致。
 * 先完成主线程上的输入校验和字符串复制，再启动 CPU 工作，避免输入复制与解析争用 CPU。
 * `Uint8Array` 省去 UTF-8 字符串复制；调用完成前不得修改它。
 */
export declare function convertMany(htmls: Array<string | Uint8Array>, options?: ConvertOptions): Promise<Page[]>

/** `convert`、`convertAsync`、`convertMany` 的选项。 */
export interface ConvertOptions {
  /** 页面地址：把相对链接转成绝对地址，也写进 `metadata.url`。 */
  url?: string
  /** 只保留正文（去掉导航、页眉页脚、侧边栏等），默认 true。 */
  onlyMainContent?: boolean
  /** 计算哪些输出，默认 `['markdown']`；只算需要的，越少越快。 */
  formats?: Format[]
}

/** 爬取里一页的结果：`page` 和 `error` 恰好有一个（失败项和 `scrapeMany` 的一样）。 */
export interface CrawlItem {
  url: string
  page?: Page
  error?: BatchError
}

/** `Scraper.crawl` 的选项：爬取范围，加上每页的抓取选项（`formats`、`onlyMainContent`、`engine`）。 */
export interface CrawlOptions {
  /** 最多抓多少页；不给时和 HTTP 接口的默认值一样。带 `adaptive` 时只是上限。 */
  limit?: number
  /** 从起始页算起最多跟几层链接。 */
  maxDepth?: number
  /** 只抓路径匹配这些正则的页面。 */
  includePaths?: Array<string>
  /** 不抓路径匹配这些正则的页面。 */
  excludePaths?: Array<string>
  /** 不限于起始页所在目录，整个主机都爬。 */
  crawlEntireDomain?: boolean
  /** 也爬同一站点的子域名。 */
  allowSubdomains?: boolean
  /** `include`（默认，sitemap 和页面链接都用）、`skip`（不读 sitemap）、`only`（只抓 sitemap 里的）。 */
  sitemap?: 'include' | 'skip' | 'only'
  /** 按和查询的相关性优先爬取。 */
  query?: string
  /** 内容覆盖查询后自动停止（需要 `query`）。 */
  adaptive?: boolean
  /** 不检查 robots.txt（整个爬取和每一页）。 */
  ignoreRobotsTxt?: boolean
  /** 这次爬取同时在抓的页面数上限。 */
  maxConcurrency?: number
  /** 每页计算哪些输出，默认 `['markdown']`。 */
  formats?: Format[]
  /** 每页只保留正文，默认 true。 */
  onlyMainContent?: boolean
  /** `auto`（默认）、`http` 或 `browser`。 */
  engine?: 'auto' | 'http' | 'browser'
}

/** 爬取结束后的说明。 */
export interface CrawlSummary {
  /**
   * `limit_reached`、`exhausted`、`cancelled`；带 `query` 时还有 `coverage_reached`、
   * `saturated`、`no_relevant_pages`。
   */
  stopReason: string
  /** 自适应爬取结束时的覆盖度（0–1）。 */
  coverage?: number
  /** 自适应爬取里每个查询词收集到的证据块数。 */
  termEvidence: Record<string, number>
  pages: number
}

/** 站点里的一个链接。 */
export interface MapLink {
  url: string
  title?: string
  description?: string
}

/** `Scraper.map` 的选项。 */
export interface MapOptions {
  /** 最多返回多少个链接；不给时和 HTTP 接口的默认值一样。 */
  limit?: number
  /** 地址里命中搜索词多的链接排在前面，没命中的保留在后面。 */
  search?: string
  /** 取值同 `CrawlOptions.sitemap`。 */
  sitemap?: 'include' | 'skip' | 'only'
  /** 不检查 robots.txt。 */
  ignoreRobotsTxt?: boolean
}

/** `parseStream` 的一个输入文件。 */
export interface ParseFile {
  /** 文件内容（`Buffer` 也可以）。 */
  data: Uint8Array
  /** 文件名：内容认不出类型时按扩展名判断，也作为结果的 `metadata.sourceURL`。 */
  filename: string
  /** 文件的 Content-Type，按内容和扩展名都认不出时再看它。 */
  contentType?: string
  /** 页面原地址：HTML 的相对链接按它解析，也作为 `metadata.sourceURL`；不会去抓。 */
  url?: string
}

/** `parse` 的选项：`ScrapeOptions` 的字段（浏览器渲染、条件请求等只对网址有意义的选项会被拒绝），加上文件的 Content-Type 和原地址。 */
export interface ParseOptions {
  /** 文件的 Content-Type，按内容和扩展名都认不出类型时再看它。 */
  contentType?: string
  /** 页面原地址：HTML 的相对链接按它解析，也作为 `metadata.sourceURL`；不会去抓。 */
  url?: string
  /** 计算哪些输出，默认 `['markdown']`；只算需要的，越少越快。 */
  formats?: Format[]
  /** 只保留正文，去掉导航、页眉页脚、侧边栏，默认 true。 */
  onlyMainContent?: boolean
  /** `auto`（默认，正文太少时改用浏览器渲染）、`http` 或 `browser`。 */
  engine?: 'auto' | 'http' | 'browser'
  /** 总超时，毫秒，至少 1000。 */
  timeout?: number
  /** 浏览器渲染时页面加载后再等待多久，毫秒。 */
  waitFor?: number
  /** 额外的请求头。 */
  headers?: Record<string, string>
  /** 这一页不检查 robots.txt（只对单页抓取有效）。 */
  ignoreRobotsTxt?: boolean
  /** 浏览器渲染时模拟手机。 */
  mobile?: boolean
  /** 浏览器渲染时屏蔽广告，默认 true。 */
  blockAds?: boolean
  /** 条件请求：`If-None-Match`。 */
  ifNoneMatch?: string
  /** 条件请求：`If-Modified-Since`（HTTP 日期）。 */
  ifModifiedSince?: string
  /** 出口代理（http://, https://, socks5://）。 */
  proxy?: string
}

/** 单页抓取选项。 */
export interface ScrapeOptions {
  /** 计算哪些输出，默认 `['markdown']`；只算需要的，越少越快。 */
  formats?: Format[]
  /** 只保留正文，去掉导航、页眉页脚、侧边栏，默认 true。 */
  onlyMainContent?: boolean
  /** `auto`（默认，正文太少时改用浏览器渲染）、`http` 或 `browser`。 */
  engine?: 'auto' | 'http' | 'browser'
  /** 总超时，毫秒，至少 1000。 */
  timeout?: number
  /** 浏览器渲染时页面加载后再等待多久，毫秒。 */
  waitFor?: number
  /** 额外的请求头。 */
  headers?: Record<string, string>
  /** 这一页不检查 robots.txt（只对单页抓取有效）。 */
  ignoreRobotsTxt?: boolean
  /** 浏览器渲染时模拟手机。 */
  mobile?: boolean
  /** 浏览器渲染时屏蔽广告，默认 true。 */
  blockAds?: boolean
  /** 条件请求：`If-None-Match`。 */
  ifNoneMatch?: string
  /** 条件请求：`If-Modified-Since`（HTTP 日期）。 */
  ifModifiedSince?: string
  /** 出口代理（http://, https://, socks5://）。 */
  proxy?: string
}

/** `Scraper` 的构造参数。 */
export interface ScraperConfig {
  /** 请求头里的 User-Agent。 */
  userAgent?: string
  /** 允许抓取内网、回环地址（默认拒绝，防 SSRF；只作用于 HTTP 抓取）。 */
  allowPrivateNetwork?: boolean
  /** 单独放行的 CIDR 网段（如 10.0.0.0/8, 198.18.0.0/15）。 */
  allowCidrs?: Array<string>
  /** 出口代理（http://, https://, socks5://）。 */
  proxy?: string
  /** 同一主机两次请求之间的最小间隔，毫秒，默认 1000；robots.txt 的 Crawl-delay 优先。 */
  requestIntervalMs?: number
  /** 同一主机同时进行的请求数，默认 2。 */
  perHostConcurrency?: number
  /** 浏览器渲染时最多启动的浏览器进程数。 */
  maxBrowsers?: number
}

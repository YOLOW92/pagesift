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

/** `scrapeMany` 里单个 URL 失败时的结果，形状和 Firecrawl 的错误响应一致。 */
export interface ErrorResult {
  success: false
  url: string
  code: ErrorCode
  error: string
  details?: unknown
  retryAfter?: number
}

export type Format = 'markdown' | 'html' | 'rawHtml' | 'links' | 'images'

/** 和 Firecrawl 的 Document 一致；没有请求的格式不会出现。 */
export interface Document {
  markdown?: string
  html?: string
  rawHtml?: string
  links?: string[]
  images?: string[]
  warning?: string
  metadata: DocumentMetadata
}

export interface DocumentMetadata {
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
  /** `scrape` 才有；`convert` 没有。 */
  statusCode?: number
  contentType?: string
  proxyUsed?: string
  /** PDF 的页数。 */
  numPages?: number
  /** pagesift 的扩展字段。 */
  pagesift?: PagesiftMetadata
  /** `ogImage`、`twitterCard`、`articlePublishedTime` 这类 meta，键名按 Firecrawl 的 camelCase 风格。 */
  [key: string]: unknown
}

export interface PagesiftMetadata {
  contentSha256?: string
  rawSha256?: string
  textSha256?: string
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

/** 抓取器：内部的连接池、限速状态和浏览器池在多次抓取之间复用。 */
export declare class Scraper {
  constructor(config?: ScraperConfig | undefined | null)
  /** 抓取一个 URL。失败时 Promise 以 `PagesiftError` 拒绝。 */
  scrape(url: string, options?: ScrapeOptions | undefined | null): Promise<Document>
  /** 抓取一批 URL，结果顺序和输入一致；单个失败以 `ErrorResult` 放进数组，不影响其他 URL。 */
  scrapeMany(urls: Array<string>, options?: ScrapeManyOptions | undefined | null): Promise<Array<Document | ErrorResult>>
  /** 关闭浏览器进程。之后再调用 `scrape` 会报错。 */
  close(): Promise<void>
}

/** 把一段 HTML 转成干净的 Markdown（在当前线程上同步执行）。 */
export declare function convert(html: string, options?: ConvertOptions | undefined | null): Document

/** 和 `convert` 相同，但在 libuv 线程池上执行，不阻塞事件循环。 */
export declare function convertAsync(html: string, options?: ConvertOptions | undefined | null): Promise<Document>

export interface ConvertOptions {
  /** 页面地址，用来把相对链接转成绝对地址，也会写进 `metadata.url`。 */
  url?: string
  /** 只保留正文（去掉导航、页眉页脚、侧边栏等），默认 true。 */
  onlyMainContent?: boolean
}

/** 用默认配置的共享 `Scraper` 抓取一个 URL（默认拒绝内网地址）。 */
export declare function scrape(url: string, options?: ScrapeOptions | undefined | null): Promise<Document>

/** 用默认配置的共享 `Scraper` 抓取一批 URL，见 `Scraper.scrapeMany`。 */
export declare function scrapeMany(urls: Array<string>, options?: ScrapeManyOptions | undefined | null): Promise<Array<Document | ErrorResult>>

/** `scrapeMany` 的选项：`ScrapeOptions` 的全部字段，加上并发数。 */
export interface ScrapeManyOptions {
  /** 同时进行的抓取数，默认 8；同一主机仍受限速约束。 */
  concurrency?: number
  /** 输出哪些内容，默认 `['markdown']`。 */
  formats?: Array<Format | { type: Format }>
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
  /** 只保留匹配这些选择器的元素（暂不支持，传非空数组会报 UNSUPPORTED_OPTION）。 */
  includeTags?: Array<string>
  /** 去掉匹配这些选择器的元素（暂不支持，传非空数组会报 UNSUPPORTED_OPTION）。 */
  excludeTags?: Array<string>
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
}

/** 单页抓取选项，字段和 Firecrawl JS SDK 的 `scrapeOptions` 一致。 */
export interface ScrapeOptions {
  /** 输出哪些内容，默认 `['markdown']`。 */
  formats?: Array<Format | { type: Format }>
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
  /** 只保留匹配这些选择器的元素（暂不支持，传非空数组会报 UNSUPPORTED_OPTION）。 */
  includeTags?: Array<string>
  /** 去掉匹配这些选择器的元素（暂不支持，传非空数组会报 UNSUPPORTED_OPTION）。 */
  excludeTags?: Array<string>
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
}

/** `Scraper` 的构造参数。 */
export interface ScraperConfig {
  /** 请求头里的 User-Agent。 */
  userAgent?: string
  /** 允许抓取内网、回环地址（默认拒绝，防 SSRF）。 */
  allowPrivateNetwork?: boolean
  /** 同一主机两次请求之间的最小间隔，毫秒，默认 1000；robots.txt 的 Crawl-delay 优先。 */
  requestIntervalMs?: number
  /** 同一主机同时进行的请求数，默认 2。 */
  perHostConcurrency?: number
  /** 浏览器渲染时最多启动的浏览器进程数。 */
  maxBrowsers?: number
}

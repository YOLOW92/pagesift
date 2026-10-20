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


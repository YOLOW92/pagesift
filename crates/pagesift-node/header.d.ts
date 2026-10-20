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


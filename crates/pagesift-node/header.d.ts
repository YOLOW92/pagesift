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

export type Format = 'markdown' | 'html' | 'rawHtml' | 'links' | 'images' | 'graph'

/** `Page.toJSON()` 的结果：一次取出全部字段的普通对象，没请求的格式不出现。 */
export interface PageJSON {
  markdown?: string
  html?: string
  rawHtml?: string
  links?: string[]
  images?: string[]
  graph?: Record<string, unknown>
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
  /** Canonical URL extracted from HTML `link[rel=canonical]` or HTTP `Link` header. */
  canonicalUrl?: string
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

/** `Scraper.sync` 请求参数。 */
export interface SyncRequest {
  action: 'create' | 'list' | 'get' | 'delete' | 'run' | 'state' | 'deltas'
  id?: string
  sourceId?: string
  name?: string
  seeds?: Array<string>
  includePaths?: Array<string>
  excludePaths?: Array<string>
  maxDepth?: number
  allowSubdomains?: boolean
  freshness?: Record<string, unknown>
  budget?: Record<string, unknown>
  maxRequestsPerTick?: number
  maxBrowserRendersPerTick?: number
  maxWallClockMs?: number
  targetItems?: number
  extractSchema?: Record<string, unknown>
  extractPrompt?: string
  requiredFields?: Array<string>
  allowBrowser?: boolean
  forceAll?: boolean
  maxRequests?: number
  asOf?: string | number
  tickId?: string
  limit?: number
}

/** `Scraper.queryGraph` 请求参数。 */
export interface GraphQueryRequest {
  historyUrl?: string
  url?: string
  claims?: boolean
  conflicts?: boolean
  subject?: string
  asOf?: string | number
  limit?: number
  offset?: number
}

/** 内联摄入或无状态检索的单文档输入。 */
export interface ContextDocumentInput {
  url: string
  markdown?: string
  title?: string
  sourceKind?: string | Record<string, unknown>
  observedAtMs?: number
  validFrom?: string
  validUntil?: string
  subjectKey?: string
  originRoot?: string
  originIndependent?: boolean
  statusCode?: number
  unreliable?: boolean
  confirmedGone?: boolean
}

/** Context 规范指纹印记（用于同 tick 规格演进重索引校验）。 */
export interface ContextSpecStamp {
  dataVersion?: number
  algorithmVersion?: number
  tokenizerVersion?: number
  modelId?: string
  evidencePolicyVersion?: number
}

/** 在线漂移受控校准反馈信号。 */
export interface ContextLearnFeedback {
  rawScore: number
  groundedCorrect?: boolean
  isHardGroundTruth?: boolean
}

/** `Scraper.context` 1.6 Context 请求参数。 */
export interface ContextRequest {
  action?:
    | 'ingest'
    | 'sync'
    | 'search'
    | 'pack'
    | 'action'
    | 'research'
    | 'verify'
    | 'watch'
    | 'changes'
    | 'units'
    | 'deltas'
    | 'ack'
    | 'learn'
  op?: ContextRequest['action']
  mode?: 'context' | 'research' | 'verify' | 'watch' | 'changes'
  query?: string
  id?: string
  sourceId?: string
  sourceIds?: Array<string>
  persist?: boolean
  specStamp?: ContextSpecStamp
  documents?: Array<ContextDocumentInput | Record<string, unknown>>
  budgetTokens?: number
  maxTokens?: number
  tokenSafetyMargin?: number
  selector?: 'submodularClosure' | 'boundedDpp' | 'mmrGreedy' | 'topKGreedy'
  ordering?: 'edgeSalience' | 'scoreDescending' | 'documentOrder'
  matchMode?: 'exactAnchorOnly' | 'greedyDice' | 'boundedUot'
  subjectKey?: string
  requiredFields?: Array<string>
  asOf?: string | number
  asOfMs?: number
  effectiveAt?: string | number
  effectiveAtMs?: number
  limit?: number
  mustKeepConflicts?: boolean
  mustKeepTableHeaders?: boolean
  mustKeepQualifiers?: boolean
  includeUnverified?: boolean
  includeConflicts?: boolean
  enableSemanticSketch?: boolean
  learnEnabled?: boolean
  url?: string
  urls?: Array<string>
  candidateUrls?: Array<string>
  minIndependentSources?: number
  maxStalenessMs?: number
  allowNetworkAcquisition?: boolean
  allowExternalSearch?: boolean
  strictConstrainToUrls?: boolean
  allowedHosts?: Array<string>
  maxAcquisitionRequests?: number
  sinceDeltaSeq?: number
  consumerId?: string
  instanceId?: string
  ackedDeltaSeq?: number
  specFingerprint?: string
  enabled?: boolean
  frozen?: boolean
  driftGuard?: 'cusumPageHinkley' | 'cusum' | 'bernoulliBocpd' | 'bocpd' | 'none'
  feedback?: ContextLearnFeedback
  search?: Record<string, unknown>
  pack?: Record<string, unknown>
  [key: string]: unknown
}

export interface ResolveLearnFeedback {
  domain?: string
  claimKind?: string
  rawScore: number
  groundedSupported: boolean
  isHardGroundTruth?: boolean
  counterProbeKind?: string
  counterFound?: boolean
  verifierVersion?: string
}

/** `Scraper.resolve` 1.7 Resolve 请求参数。 */
export interface ResolveRequest {
  action?:
    | 'check'
    | 'verify'
    | 'conclusions'
    | 'list'
    | 'get'
    | 'history'
    | 'graph'
    | 'reconcile'
    | 'revalidate'
    | 'events'
    | 'learn'
    | 'learn_status'
    | 'delete'
  op?: ResolveRequest['action']
  answer?: string
  text?: string
  claims?: Array<string>
  query?: string
  domain?: string
  conclusionId?: string
  conclusionIds?: Array<string>
  id?: string
  sourceId?: string
  sourceIds?: Array<string>
  documents?: Array<ContextDocumentInput | Record<string, unknown>>
  parentConclusionIds?: Array<string>
  assumptions?: Array<string>
  proofStandard?: 'scintilla' | 'preponderance' | 'clear_and_convincing' | 'strict_universal'
  domainKind?: 'open_world' | 'closed_observed_set'
  verifierMode?: 'deterministic_only' | 'hybrid_assisted'
  minIndependentSources?: number
  asOf?: string | number
  asOfMs?: number
  effectiveAt?: string | number
  effectiveAtMs?: number
  includeUnverified?: boolean
  enableGapResolution?: boolean
  enableCounterSearch?: boolean
  allowNetworkAcquisition?: boolean
  allowExternalSearch?: boolean
  strictConstrainToUrls?: boolean
  candidateUrls?: Array<string>
  allowedHosts?: Array<string>
  maxAcquisitionRequests?: number
  persist?: boolean
  learnEnabled?: boolean
  mode?: 'incremental' | 'full_source' | 'full'
  state?: string
  version?: number
  afterSeq?: number
  limit?: number
  enabled?: boolean
  frozen?: boolean
  reset?: boolean
  verifierVersion?: string
  feedback?: Array<ResolveLearnFeedback>
  reason?: string
  [key: string]: unknown
}





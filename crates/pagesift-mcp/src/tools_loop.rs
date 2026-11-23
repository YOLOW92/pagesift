//! 1.5 Loop MCP 工具定义：`pagesift_sync`（长期数据源同步）与扩展版 `pagesift_graph_query`。

use serde_json::json;

use crate::output;
use crate::tool::{ToolAnnotations, ToolDef};

pub const GRAPH_QUERY: &str = "pagesift_graph_query";
pub const SYNC: &str = "pagesift_sync";
pub const CONTEXT: &str = "pagesift_context";
pub const RESOLVE: &str = "pagesift_resolve";

pub fn graph_query_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "jobId": { "type": "string", "description": "Crawl job id from pagesift_crawl to summarize its DataGraph." },
            "historyUrl": { "type": "string", "description": "URL or normalized resource key to query historical versions and diff edges." },
            "conflicts": { "type": "boolean", "description": "When true, return detected cross-source conflicts instead of a single job or URL." },
            "claims": { "type": "boolean", "description": "When true, return persisted claim lifecycle records instead of conflicts." },
            "subject": { "type": "string", "description": "Optional subject key filter for conflicts or claims (e.g. sku:PRO-1); implies conflicts=true when set alone." },
            "asOf": { "type": "string", "description": "Optional historical timestamp cutoff (Unix milliseconds string or RFC-3339 date) for time-travel queries." },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of history, conflict, or claim records to return." },
            "offset": { "type": "integer", "minimum": 0, "description": "Pagination offset for conflict or claim queries." }
        },
        "additionalProperties": false
    });
    ToolDef::new(GRAPH_QUERY, schema)
        .title("Query DataGraph")
        .description(
            "Query PageSift DataGraph results produced during crawls or archived in the store: \
             pass jobId for a crawl's GraphSummary, historyUrl for cross-time version lineage, \
             conflicts/subject for cross-source conflicts, or claims=true with optional asOf for claim lifecycle state.",
        )
        .output_schema(output::graph_query())
        .annotations(ToolAnnotations {
            title: Some("pagesift graph query".into()),
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        })
}

pub fn sync_def() -> ToolDef {
    let schema = json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["create", "list", "get", "delete", "run", "state", "deltas"],
                "description": "Operation to perform on persistent Loop data sources: create, list, get, delete, run (trigger one sync tick), state (view resource inventory and active claims), or deltas (inspect tick deltas)."
            },
            "sourceId": { "type": "string", "description": "Identifier of the data source (required for get, delete, run, state, deltas; optional for create)." },
            "name": { "type": "string", "description": "Optional human-readable label when action='create'." },
            "seeds": {
                "type": "array",
                "items": { "type": "string", "format": "uri", "description": "Seed HTTP(S) URL for this data source." },
                "description": "Seed URLs defining the starting points and scope hosts for action='create'."
            },
            "includePaths": {
                "type": "array",
                "items": { "type": "string", "description": "URL path prefix or pattern to include." },
                "description": "Optional path prefixes to keep in scope for action='create'."
            },
            "excludePaths": {
                "type": "array",
                "items": { "type": "string", "description": "URL path prefix or pattern to exclude." },
                "description": "Optional path prefixes to exclude from scope for action='create'."
            },
            "maxDepth": { "type": "integer", "minimum": 0, "description": "Maximum discovery depth from seeds for action='create'." },
            "allowSubdomains": { "type": "boolean", "description": "Whether subdomains of seed hosts are allowed in scope for action='create'." },
            "requiredFields": {
                "type": "array",
                "items": { "type": "string", "description": "Required structured field name." },
                "description": "Structured fields required for capture sufficiency checks and adaptive browser escalation."
            },
            "allowBrowser": { "type": "boolean", "description": "Allow adaptive escalation to headless browser rendering during sync ticks." },
            "maxRequestsPerTick": { "type": "integer", "minimum": 1, "description": "Maximum HTTP/browser requests allowed per sync tick when action='create'." },
            "targetItems": { "type": "integer", "minimum": 1, "description": "Optional target count of substantive items for target_goal_satisfied coverage evaluation." },
            "forceAll": { "type": "boolean", "description": "When action='run', refresh discovered resources even if freshness timer has not elapsed." },
            "maxRequests": { "type": "integer", "minimum": 1, "description": "Override request budget for this single action='run' tick." },
            "asOf": { "type": "string", "description": "When action='state', evaluate resource inventory, claims, and conflicts as of a Unix ms string or RFC-3339 date." },
            "tickId": { "type": "string", "description": "When action='deltas', return one specific tick delta instead of the recent list." },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of deltas to list when action='deltas'." }
        },
        "additionalProperties": false
    });
    ToolDef::new(SYNC, schema)
        .title("Manage Loop Sync Sources")
        .description(
            "Create, run, inspect, and manage long-lived PageSift Loop data sources: schedule adaptive \
             incremental sync ticks, inspect bounded sync deltas, and query resource/claim state views.",
        )
        .output_schema(output::sync_result())
        .annotations(ToolAnnotations {
            title: Some("pagesift sync".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
}

pub fn context_def() -> ToolDef {
    let doc_item = json!({
        "type": "object",
        "required": ["url", "markdown"],
        "properties": {
            "url": { "type": "string", "description": "Document source URL or logical identifier." },
            "markdown": { "type": "string", "description": "Extracted Markdown body of the document." },
            "title": { "type": "string", "description": "Optional document title." },
            "sourceKind": { "description": "Optional document format kind (e.g. 'html', 'pdf', 'docx')." },
            "observedAtMs": { "type": "integer", "minimum": 0, "description": "Observation timestamp in Unix milliseconds." },
            "validFrom": { "type": "string", "description": "Optional ISO-8601 / RFC-3339 effective start timestamp." },
            "validUntil": { "type": "string", "description": "Optional ISO-8601 / RFC-3339 effective end timestamp." },
            "subjectKey": { "type": "string", "description": "Optional explicit entity subject key." },
            "originRoot": { "type": "string", "description": "Optional independent evidentiary origin root key." },
            "originIndependent": { "type": "boolean", "description": "Whether this document is an independent source rather than a mirror or derivative." },
            // [协议] RFC 9110 §15：标准 HTTP 响应状态码区间为 100..=599
            "statusCode": { "type": "integer", "minimum": 100, "maximum": 599, "description": "Optional HTTP status code associated with this observation." },
            "unreliable": { "type": "boolean", "description": "Mark capture as truncated or decode-uncertain so existing units become unverified instead of withdrawn." },
            "confirmedGone": { "type": "boolean", "description": "Mark resource as confirmed gone to retire its active units." }
        },
        "additionalProperties": false,
        "description": "Inline document input for Context ingestion or stateless compilation."
    });

    let spec_stamp = json!({
        "type": "object",
        "properties": {
            "dataVersion": { "type": "integer", "minimum": 1, "description": "Schema/data representation version." },
            "algorithmVersion": { "type": "integer", "minimum": 1, "description": "Unit alignment and indexing algorithm version." },
            "tokenizerVersion": { "type": "integer", "minimum": 1, "description": "Lexical/symbol tokenizer normalization version." },
            "modelId": { "type": "string", "description": "Embedding or scoring model identifier." },
            "evidencePolicyVersion": { "type": "integer", "minimum": 1, "description": "Evidence policy version." }
        },
        "additionalProperties": false,
        "description": "Multi-dimensional configuration stamp governing derived-asset reuse vs rebuild."
    });

    let learn_feedback = json!({
        "type": "object",
        "properties": {
            "rawScore": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Raw uncalibrated retrieval score in [0, 1]." },
            "groundedCorrect": { "type": "boolean", "description": "Whether grounded evidence verification succeeded." },
            "isHardGroundTruth": { "type": "boolean", "description": "True only for externally verified ground-truth labels (weak signals are ignored by isotonic fitting)." }
        },
        "additionalProperties": false,
        "description": "Supervised calibration feedback sample when action='learn'."
    });

    let mut props = serde_json::Map::new();
    if let serde_json::Value::Object(core_props) = json!({
        "action": {
            "type": "string",
            "enum": ["ingest", "sync", "search", "pack", "action", "research", "verify", "watch", "changes", "units", "deltas", "ack", "learn"],
            "description": "Context operation: pack (default: search + budget-constrained ContextPack), search, ingest, sync (catch up from Loop source), action/research/verify/watch/changes, units, deltas, ack, or learn."
        },
        "op": { "type": "string", "description": "Alias for 'action'." },
        "mode": {
            "type": "string",
            "enum": ["context", "research", "verify", "watch", "changes"],
            "description": "Goal-oriented mode when action='action'."
        },
        "query": { "type": "string", "description": "Information need, exact symbol, or question to retrieve and compile context for." },
        "sourceId": { "type": "string", "description": "Target Context or Loop source identifier." },
        "id": { "type": "string", "description": "Alias for 'sourceId'." },
        "sourceIds": {
            "type": "array",
            "items": { "type": "string", "description": "Source identifier." },
            "description": "Multiple Context source identifiers to search across."
        },
        "persist": { "type": "boolean", "description": "Persist ingested units to Store when action='ingest' (defaults to true when Store is configured)." },
        "specStamp": spec_stamp,
        "documents": {
            "type": "array",
            "items": doc_item,
            "description": "Optional inline documents for ingest or stateless search/pack without a persistent Store."
        },
        "url": { "type": "string", "description": "Optional resource URL filter when action='units'." },
        "subjectKey": { "type": "string", "description": "Optional entity subject key filter." },
        "requiredFields": {
            "type": "array",
            "items": { "type": "string", "description": "Required field name." },
            "description": "Fields that must have verified evidence support."
        },
        "asOf": { "type": "string", "description": "Point-in-time observation cutoff (Unix ms string or RFC-3339 timestamp) preventing future leakage." },
        "asOfMs": { "type": "integer", "minimum": 0, "description": "Point-in-time observation cutoff in Unix milliseconds." },
        "effectiveAt": { "type": "string", "description": "Target factual validity timestamp (Unix ms string or RFC-3339 timestamp) for temporal RAG." },
        "effectiveAtMs": { "type": "integer", "minimum": 0, "description": "Target factual validity timestamp in Unix milliseconds." },
        "search": { "type": "object", "description": "Optional nested ContextSearchRequest object when passing structured requests." },
        "pack": { "type": "object", "description": "Optional nested ContextPackRequest object when passing structured requests." }
    }) {
        props.extend(core_props);
    }

    if let serde_json::Value::Object(pack_props) = json!({
        "budgetTokens": { "type": "integer", "minimum": 1, "maximum": 1048576, "description": "Maximum token budget for ContextPack compilation (default 2048)." },
        "maxTokens": { "type": "integer", "minimum": 1, "maximum": 1048576, "description": "Alias for 'budgetTokens'." },
        "tokenSafetyMargin": { "type": "number", "minimum": 0.0, "maximum": 0.5, "description": "Fractional token reserve in [0, 0.5] guarding against tokenizer drift (default 0.05)." },
        "mustKeepConflicts": { "type": "boolean", "description": "Keep opposing active conflict evidence paired in ContextPack (default true)." },
        "mustKeepTableHeaders": { "type": "boolean", "description": "Keep table headers and captions bound to selected table rows (default true)." },
        "mustKeepQualifiers": { "type": "boolean", "description": "Preserve negations, units, and qualifier clauses (default true)." },
        "selector": {
            "type": "string",
            "enum": ["submodularClosure", "boundedDpp", "mmrGreedy", "topKGreedy"],
            "description": "ContextPack selection optimizer."
        },
        "ordering": {
            "type": "string",
            "enum": ["edgeSalience", "scoreDescending", "documentOrder"],
            "description": "Section ordering inside compiledContext (edgeSalience places top evidence at boundaries)."
        },
        "matchMode": {
            "type": "string",
            "enum": ["boundedUot", "greedyDice", "exactAnchorOnly"],
            "description": "Cross-version unit identity reconciliation strategy."
        },
        // [策略] MCP Context 检索/枚举返回条数硬上界 10_000（与引擎 MAX_CONTEXT_RESULT_LIMIT 一致）
        "limit": { "type": "integer", "minimum": 1, "maximum": 10000, "description": "Maximum candidate hits, units, or deltas to return." },
        "includeUnverified": { "type": "boolean", "description": "Include units marked unverified after transient errors or barriers." },
        "includeConflicts": { "type": "boolean", "description": "Co-retrieve opposing conflict evidence variants." },
        "enableSemanticSketch": { "type": "boolean", "description": "Enable deterministic character trigram semantic sketch fallback (default true)." },
        "learnEnabled": { "type": "boolean", "description": "Enable calibrated confidence scoring (false uses deterministic baseline)." }
    }) {
        props.extend(pack_props);
    }

    if let serde_json::Value::Object(action_props) = json!({
        "candidateUrls": {
            "type": "array",
            "items": { "type": "string", "format": "uri", "description": "Candidate HTTP(S) URL." },
            "description": "Candidate URLs for bounded acquisition in research/verify/watch modes."
        },
        "urls": {
            "type": "array",
            "items": { "type": "string", "format": "uri", "description": "Candidate HTTP(S) URL." },
            "description": "Alias for 'candidateUrls'."
        },
        "allowedHosts": {
            "type": "array",
            "items": { "type": "string", "description": "Allowed hostname." },
            "description": "Strict host allowlist for network acquisition."
        },
        "allowNetworkAcquisition": { "type": "boolean", "description": "Explicitly permit bounded Loop/scrape acquisition when existing Context has coverage gaps." },
        "allowExternalSearch": { "type": "boolean", "description": "Permit configured external SearchBackend queries during research." },
        "strictConstrainToUrls": { "type": "boolean", "description": "Restrict acquisition strictly to candidateUrls and managed sources." },
        // [策略] MCP 单次 Action 补充采集硬上限 64 请求、独立源阈值硬上限 256（与引擎 validate.rs 边界一致）
        "maxAcquisitionRequests": { "type": "integer", "minimum": 0, "maximum": 64, "description": "Hard cap on network requests during one Action call." },
        "minIndependentSources": { "type": "integer", "minimum": 1, "maximum": 256, "description": "Minimum independent origin roots required before stopping." },
        "maxStalenessMs": { "type": "integer", "minimum": 0, "description": "Maximum acceptable age of existing observations in milliseconds." },
        "sinceDeltaSeq": { "type": "integer", "minimum": 0, "description": "Return ContextDeltas strictly after this sequence number in changes mode." },
        "consumerId": { "type": "string", "description": "External index consumer identifier for deltas/ack." },
        "instanceId": { "type": "string", "description": "ABA-guarded source instanceId when acknowledging deltas." },
        "ackedDeltaSeq": { "type": "integer", "minimum": 0, "description": "Highest processed deltaSeq when action='ack'." },
        "specFingerprint": { "type": "string", "description": "AssetSpecStamp fingerprint expected by external consumer." },
        "enabled": { "type": "boolean", "description": "Enable or disable online Context calibration when action='learn'." },
        "frozen": { "type": "boolean", "description": "Freeze or unfreeze online Context calibration when action='learn'." },
        "driftGuard": {
            "type": "string",
            "enum": ["cusumPageHinkley", "cusum", "bernoulliBocpd", "bocpd", "none"],
            "description": "Online distribution-drift detector guarding calibration state when action='learn'."
        },
        "feedback": learn_feedback
    }) {
        props.extend(action_props);
    }

    let schema = json!({
        "type": "object",
        "properties": serde_json::Value::Object(props),
        "additionalProperties": false
    });

    ToolDef::new(CONTEXT, schema)
        .title("Evidence Context Infrastructure")
        .description(
            "Build, search, incrementally maintain, and compile evidence-grounded AI Context: \
             stable cross-version KnowledgeUnits, dependency-tracked ContextDeltas, temporal/conflict-aware \
             multi-strategy retrieval, token-budgeted ContextPacks with provenance manifests, and VOI-governed acquisition.",
        )
        .output_schema(output::context_result())
        .annotations(ToolAnnotations {
            title: Some("pagesift context".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
}

pub fn resolve_def() -> ToolDef {
    let doc_item = json!({
        "type": "object",
        "required": ["url", "markdown"],
        "properties": {
            "url": { "type": "string", "description": "Document source URL or logical identifier." },
            "markdown": { "type": "string", "description": "Extracted Markdown body of the document." },
            "title": { "type": "string", "description": "Optional document title." },
            "observedAtMs": { "type": "integer", "minimum": 0, "description": "Observation timestamp in Unix milliseconds." },
            "validFrom": { "type": "string", "description": "Optional RFC-3339 effective start timestamp." },
            "validUntil": { "type": "string", "description": "Optional RFC-3339 effective end timestamp." },
            "subjectKey": { "type": "string", "description": "Optional entity subject key." },
            "originRoot": { "type": "string", "description": "Optional independent evidentiary origin root key." },
            "originIndependent": { "type": "boolean", "description": "Whether this document is an independent source rather than a mirror or reprint." }
        },
        "additionalProperties": false
    });

    let feedback_item = json!({
        "type": "object",
        "required": ["rawScore", "groundedSupported"],
        "properties": {
            "domain": { "type": "string", "description": "Domain bucket label (default: 'general')." },
            "claimKind": { "type": "string", "description": "Claim quantifier kind label." },
            "rawScore": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Raw alignment score in [0, 1]." },
            "groundedSupported": { "type": "boolean", "description": "Externally verified ground-truth support label." },
            "isHardGroundTruth": { "type": "boolean", "description": "Must be true for calibration updates; weak signals are ignored." },
            "counterProbeKind": { "type": "string", "description": "Optional counter probe kind." },
            "counterFound": { "type": "boolean", "description": "Whether counter probe surfaced relevant counter-evidence." },
            "verifierVersion": { "type": "string", "description": "Verifier version tag." }
        },
        "additionalProperties": false
    });

    let mut props = serde_json::Map::new();
    if let serde_json::Value::Object(core_props) = json!({
        "action": {
            "type": "string",
            "enum": ["check", "verify", "conclusions", "list", "get", "history", "graph", "reconcile", "revalidate", "events", "learn", "learn_status", "delete"],
            "description": "Resolve operation: check (default), conclusions, get, history, graph, reconcile, revalidate, events, learn, learn_status, or delete."
        },
        "op": { "type": "string", "description": "Alias for 'action'." },
        "answer": { "type": "string", "description": "Candidate natural-language answer to decompose into atomic claims and verify." },
        "text": { "type": "string", "description": "Alias for 'answer'." },
        "claims": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional explicit atomic claim strings to verify."
        },
        "query": { "type": "string", "description": "Optional original question context." },
        "domain": { "type": "string", "description": "Optional domain bucket for calibration and auditing." },
        "conclusionId": { "type": "string", "description": "Target conclusion identifier for get, history, graph, revalidate, delete, or updating an existing conclusion." },
        "conclusionIds": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Explicit conclusion IDs to revalidate."
        },
        "sourceId": { "type": "string", "description": "Single Context/Loop source ID filter." },
        "sourceIds": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Context/Loop source IDs to verify against or reconcile."
        },
        "documents": {
            "type": "array",
            "items": doc_item,
            "description": "Optional inline documents for ephemeral stateless verification."
        },
        "parentConclusionIds": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Upstream conclusion IDs this conclusion depends on (cycles are rejected)."
        },
        "assumptions": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Explicit assumptions required by this conclusion."
        },
        "proofStandard": {
            "type": "string",
            "enum": ["scintilla", "preponderance", "clear_and_convincing", "strict_universal"],
            "description": "Carneades-style proof standard (default: preponderance)."
        },
        "domainKind": {
            "type": "string",
            "enum": ["open_world", "closed_observed_set"],
            "description": "Whether the queried corpus represents an open world (default) or a closed observed set."
        },
        "verifierMode": {
            "type": "string",
            "enum": ["deterministic_only", "hybrid_assisted"],
            "description": "Deterministic core only (default) or optional quote-grounded LLM assistance on inconclusive claims."
        },
        // [策略] 独立信源数上限 256、补采请求上限 64、分页上限 1000 与引擎 validate.rs 一致
        "minIndependentSources": { "type": "integer", "minimum": 1, "maximum": 256, "description": "Minimum independent origin roots required." },
        "asOf": { "type": "string", "description": "Observation time cutoff (Unix ms or RFC-3339) preventing future evidence leakage." },
        "effectiveAt": { "type": "string", "description": "Business validity target time (Unix ms or RFC-3339)." },
        "includeUnverified": { "type": "boolean", "description": "Whether to consider unverified/unreliable Context units." }
    }) {
        props.extend(core_props);
    }

    if let serde_json::Value::Object(extra_props) = json!({
        "enableGapResolution": { "type": "boolean", "description": "Enable MCS+VOI prioritized gap resolution (default: true)." },
        "enableCounterSearch": { "type": "boolean", "description": "Enable proactive counter-evidence probing and coverage audit (default: true)." },
        "allowNetworkAcquisition": { "type": "boolean", "description": "Explicitly permit bounded network acquisition via 1.6 ContextAction guardrails (default: false)." },
        "allowExternalSearch": { "type": "boolean", "description": "Permit configured external search queries during gap/counter acquisition (default: false)." },
        "strictConstrainToUrls": { "type": "boolean", "description": "Restrict network acquisition strictly to candidateUrls and managed sources." },
        "candidateUrls": {
            "type": "array",
            "items": { "type": "string", "format": "uri" },
            "description": "Candidate URLs for bounded gap resolution."
        },
        "allowedHosts": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Strict host allowlist enforced across HTTP, redirects, and browser sub-requests."
        },
        // [策略] Resolve 单次网络补采请求硬上限 64（与引擎 validate.rs 边界一致）
        "maxAcquisitionRequests": { "type": "integer", "minimum": 0, "maximum": 64, "description": "Hard cap on network requests during one Resolve call (default: 4)." },
        "persist": { "type": "boolean", "description": "Persist versioned ConclusionRecord into Store ns::RESOLVE with reverse dependency & scope indexes." },
        "learnEnabled": { "type": "boolean", "description": "Whether to attach calibrated probability when domain calibration is warm." },
        "mode": { "type": "string", "enum": ["incremental", "full_source", "full"], "description": "Reconcile or revalidate mode." },
        "state": { "type": "string", "description": "Optional ResolutionState filter when listing conclusions." },
        "version": { "type": "integer", "minimum": 1, "description": "Specific historical revision version for action='get'." },
        "afterSeq": { "type": "integer", "minimum": 0, "description": "Return ResolveEvents strictly after this monotonic sequence." },
        "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Maximum records returned." },
        "enabled": { "type": "boolean", "description": "Enable or disable Resolve calibration when action='learn'." },
        "frozen": { "type": "boolean", "description": "Freeze or unfreeze Resolve calibration when action='learn'." },
        "reset": { "type": "boolean", "description": "Reset Resolve learn state when action='learn'." },
        "verifierVersion": { "type": "string", "description": "Verifier version check/reset when action='learn'." },
        "feedback": { "type": "array", "items": feedback_item, "description": "Hard ground-truth feedback samples for action='learn'." },
        "reason": { "type": "string", "description": "Audit reason when action='delete'." }
    }) {
        props.extend(extra_props);
    }

    let schema = json!({
        "type": "object",
        "properties": serde_json::Value::Object(props),
        "additionalProperties": false
    });

    ToolDef::new(RESOLVE, schema)
        .title("Evidence-Driven Resolution Engine")
        .description(
            "Verify answers and claims against grounded Context evidence with context-preserving claim \
             decomposition, Belnap four-valued verdicts, Carneades proof standards, common-cause origin \
             collapse, Minimal Cut Set vulnerability analysis, proactive counter-evidence probing, \
             bounded VOI gap resolution, and Live incremental conclusion maintenance.",
        )
        .output_schema(output::resolve_result())
        .annotations(ToolAnnotations {
            title: Some("pagesift resolve".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(true),
        })
}

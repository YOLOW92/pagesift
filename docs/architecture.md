# Architecture

The product is one Rust binary. Python and Node embed the same engine. A browser for JavaScript pages, an NVIDIA GPU
for OCR and a model for `/v2/agent` are optional and found at run time.

## Request pipeline

```mermaid
flowchart TD
    CLI["CLI / Rust caller"] --> Engine["pagesift-engine: scrape, crawl, Loop sync, Context"]
    HTTP["HTTP API: Firecrawl, Jina, /v2/sources, /v2/graph, /v2/context"] --> Engine
    Bindings["Python / Node / MCP / C ABI: Go, Java, C#, Ruby, PHP and more"] --> Engine
    Engine --> Hooks["pagesift-hooks: rules and hooks"]
    Hooks --> Policy["pagesift-policy: robots, rate limits, backoff, identity leases"]
    Policy --> Net["pagesift-net: HTTP, SSRF guards, validated proxy tunnels, optional TLS fingerprint"]
    Net --> Plan{"pagesift-plan: strategy, render?"}
    Plan -->|HTTP or embedded JSON| DOM["pagesift-html (yolowhtml-dom): single-pass arena DOM"]
    Plan -->|Browser| CDP["pagesift-cdp: browser pool, automation-marker removal, challenge settle"]
    CDP --> DOM
    Net -->|PDF| PDF["pagesift-pdf (yolowpdf + yolow64)"]
    Net -->|Office, e-books, email, archives, text| Office["pagesift-office (yolowdoc)"]
    Net -->|Image| OCR["pagesift-ocr, pagesift-infer, pagesift-gpu"]
    Office -->|archive member, attachment| Office
    Office -->|archive member, attachment| PDF
    Office -->|source code| Syntax["pagesift-syntax (yolowsyntax)"]
    PDF -->|scanned page| OCR
    DOM --> Frontier["pagesift-frontier: URL scoring, link usefulness"]
    Frontier -.->|next crawl, sync URLs| Engine
    DOM --> Content["pagesift-content (yolowhtml): main content, Markdown, metadata"]
    DOM --> Extract["pagesift-extract: selectors, field evidence, chunking"]
    Content --> Graph["pagesift-graph (DataGraph): citations, claim lifecycle, independent roots, conflicts, asOf"]
    Extract --> Graph
    PDF --> Graph
    Office --> Graph
    Graph --> Sink["pagesift-sink: outputs, graph.jsonl, cache format"]
    PDF --> Sink
    Office --> Sink
    OCR --> Sink
    Content --> Sink
    Extract --> Sink
    Graph --> Store["pagesift-store: inventories, claim history, Context units & deltas (ns::CONTEXT + chis), experience, cache, snapshots"]
    Graph --> Ctx["Context (pagesift-graph + pagesift-extract + pagesift-stat): KnowledgeUnit (BoundedUot), ContextDelta, multi-strategy Search, ContextPack, VOI Action & Learn"]
    Ctx <--> Store
    Ctx -.->|VOI + 4D shadow-price bounded acquisition| Engine
    Engine <-->|SyncDelta, ContextDelta, assessments, snapshots| Store
    Store -.->|family experience, URL rewrites| Plan
    Net --> Compress["pagesift-compress (yolowzstd): HTTP and cache zstd codecs"]
    Store --> Compress
    Engine -.-> ITNA["pagesift-itna: barrier classification, attempt and network budgets"]
    ITNA -.->|shared budget| Net
    ITNA -.->|one bounded fallback| CDP
    Net -.->|restricted cookie handoff| CDP
```

Access-barrier recovery is opt-in (`[itna] enabled`, off by default). `pagesift-itna` classifies barriers,
allows one bounded browser fallback, and shares one network budget across redirects and browser sub-requests.
Sessions are leased per owner, origin, egress and fingerprint; HTTP cookies reach the browser only through an
exact-origin handoff. Interactive CAPTCHAs are reported as needing a human and are not solved.

The HTTP layer validates options before execution. `"explain": true` (`scrape --explain`) writes every render decision
to `metadata.pagesift.plan.steps`.

When DataGraph is enabled (`"graph"` format or `--graph`), `pagesift-graph` links extracted blocks, citations,
redirect/canonical/attachment provenance, cross-version lineage, field-level evidence spans, cross-source fact clusters,
and conflict records across single scrapes and multi-page crawls (`graph.jsonl`, `graph_summary.json`, HTTP `/v2/crawl/{id}/graph`,
`/v2/graph/history`, `/v2/graph/conflicts`, `/v2/graph/claims`, `?asOf=` historical reconstruction, and MCP `pagesift_graph_query`), while tracking claim lifecycles (`Active`, `Superseded`, `Withdrawn`, `Unverified`, `EvidenceDegraded`) and independent source roots (`OriginRelation`).

Long-lived **Loop** collection ties `pagesift-store` source inventories (`DataSourceSpec`, `SyncDelta`, `SourceStateView`), `pagesift-plan` cross-tick `FamilyExperienceTable` statistics and explainable `AcquisitionStrategy` planning, and `pagesift-engine` incremental `sync_source` execution into `/v2/sources`, `pagesift sync`, `pagesift_sync`, and the Node.js / Python / C ABI bindings. Large captured monitor Markdown payloads are offloaded to content-addressed snapshots so monitor state metadata stays compact.

On top of `DataGraph` and `Loop`, **Context** ([docs/context.md](context.md)) maintains cross-version `KnowledgeUnit` identities (separating `unitId`, `revisionId`, physical byte/page/table `location`, and `lineage` with explicit ambiguity records), commits atomic dependency-governed `ContextDelta` updates and idempotent consumer checkpoints in `pagesift-store` (`ns::CONTEXT`, immune to cache eviction, with `chis` point-in-time resource history), runs multi-strategy lexical/symbol/temporal/lineage `Search`, compiles token- and `MustKeep`-constrained `ContextPack` outputs with `provenanceManifest`, and executes bounded goal-oriented `Action` workflows (`context`, `research`, `verify`, `watch`, `changes`) governed by gated coverage diagnosis, sequential Value of Information, 4D admission shadow prices, and drift-guarded online calibration (`pagesift context`, `/v2/context`, `pagesift_context`, and SDK `context()`).

```mermaid
flowchart LR
    U["Unit (pagesift-extract)<br/>Patience anchors + entity guards<br/>+ BoundedUot / GreedyDice"]
    D["Delta (pagesift-store ns::CONTEXT)<br/>AtomicTx, AssetDependency stamps,<br/>chis history, anti-ABA checkpoints"]
    S["Search (pagesift-extract)<br/>BM25 + symbols + headings,<br/>asOf & effectiveAt, conflict pairs"]
    P["Pack (pagesift-extract)<br/>SubmodularClosure / BoundedDpp,<br/>MustKeep closures, EdgeSalience"]
    A["Action (pagesift-engine + stat)<br/>Gated Good-Turing / Chapman,<br/>Howard VOI vs 4D shadow prices"]
    L["Learn (pagesift-stat)<br/>Grounded PAV isotonic calibration,<br/>CusumPageHinkley / Bocpd guard"]

    U --> D --> S --> P --> A --> L
    A -.->|bounded sync / fetch when VOI > cost| U
    L -.->|calibrated confidence| S
```

## Crates

| Area | Crates |
|---|---|
| Entry points | `pagesift` (CLI and library), `pagesift-api` (HTTP), `pagesift-mcp`, `pagesift-py`, `pagesift-node`, `pagesift-ffi` |
| Engine | `pagesift-engine`, `pagesift-plan`, `pagesift-hooks`, `pagesift-llm`, `pagesift-search`, `pagesift-graph` |
| Network and recovery | `pagesift-net`, `pagesift-policy` (rate limits and identity leases), `pagesift-rustls`, `pagesift-cdp`, `pagesift-media`, `pagesift-itna` |
| Content | `pagesift-html` (`yolowhtml-dom`), `pagesift-content` (`yolowhtml`), `pagesift-extract`, `pagesift-diff`, `pagesift-frontier` |
| Documents | `pagesift-pdf` (`yolowpdf`), `pagesift-office` (`yolowdoc`), `pagesift-syntax` (`yolowsyntax`), `pagesift-ocr`, `pagesift-infer`, `pagesift-gpu` |
| Runtime | `pagesift-flow` (workers and admission), `pagesift-stat` (online statistics), `pagesift-base64` (`yolow64`), `pagesift-compress` (`yolowzstd`), `pagesift-sys`, `pagesift-store`, `pagesift-sink`, `pagesift-cluster` |

## Execution

CPU work runs on one resident worker per core, grouped by NUMA node, stealing inside the node first. Admission is one
path for CPU, memory and browser tabs, and capacity is sized from the cgroup limit and measured browser memory.

## Measured values

Most limits start from a safe default and move only when the evidence is statistically significant (δ = 0.05). Global
concurrency follows a Universal Scalability Law fit, per-host concurrency a latency gradient, stage timeouts
Kaplan–Meier survival, retries per-class success rates. Whether to render is learned per path family by Thompson
sampling. User settings, safety limits and protocol values stay hard limits.
[Every replaced value](history/0.86_fixed_values.md)

## Several servers

`serve --role gateway`, `worker` and `standby` run from the same binary. Sites are assigned to workers by rendezvous
hashing, the standby takes over through leases with fencing, and jobs and webhooks resume after a restart.
Browser sessions (`/interact`) are not available through the gateway.

## Incremental delivery

```mermaid
stateDiagram-v2
    [*] --> FetchPage
    FetchPage --> Identify: map source + stable item ID
    Identify --> CompareRevision: reject ambiguous identities
    CompareRevision --> Skip: revision already acknowledged
    CompareRevision --> Deliver: new or changed revision
    Deliver --> Acknowledge: consumer accepts item
    Acknowledge --> CommitCheckpoint: persist revision + pagination
    CommitCheckpoint --> FetchPage: next page
    CommitCheckpoint --> Complete: terminal page
    Deliver --> FetchPage: restart before acknowledgement may replay
    Skip --> CommitCheckpoint
    Complete --> [*]
```

Delivery is **at least once**. The caller acknowledges after delivery; a crash before the commit can replay an item. Opt-in deletion sync runs only after a successful terminal traversal. URL cache eviction does not erase business completion records.

## Verified media publication

```mermaid
flowchart LR
    ID[Source / item / revision] --> Lock[Exclusive archive writer]
    Lock --> Manifest[Inspect manifest + partial checksum]
    Manifest --> Range{Strong validator matches?}
    Range -->|Yes| Resume[Validated byte-range resume]
    Range -->|No| Fresh[Restart without mixing revisions]
    Resume --> Verify[Verify complete bytes + checksum]
    Fresh --> Verify
    Verify --> Media[Atomically publish media]
    Media --> Sidecar[Publish metadata sidecar]
    Sidecar --> Done[Commit completion record last]
```

A retry reconciles partial publication before declaring completion. Filenames are sanitized and collisions rejected. Finite HLS/DASH planning reuses this archive layer for bounded segments; DASH retains timing/addressing in a local MPD. It does not mux tracks, execute FFmpeg or support encrypted/live streams.

See [collection](incremental-collection.md), [archives](media-archive.md) and [bounded core APIs](native-core.md) for exact limits and failure behavior.

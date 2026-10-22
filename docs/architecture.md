# Architecture

The product core and all new 2.0 modules are Rust. Python and Node embed that engine; benchmark drivers use their existing Python/Node environments. Optional browser rendering is separate from persistent interaction or autonomous Agent workflows.

![Engine overview](assets/overview.svg)

## Request pipeline

```mermaid
flowchart TD
    CLI[CLI / Rust caller] --> Engine[pagesift engine]
    HTTP[Firecrawl-compatible HTTP API] --> Engine
    Bindings[Native Python / Node + MCP] --> Engine
    Engine --> Policy[Deadline / origin policy / robots / rate limits]
    Policy --> Net[pagesift-net: HTTP and explicit Cookie sessions]
    Net --> Decide{Rendering needed?}
    Decide -->|No| DOM[pagesift-html: HTML5 DOM]
    Decide -->|Yes| CDP[pagesift-cdp: optional browser pool]
    CDP --> DOM
    DOM --> Content[pagesift-content: article / Markdown / metadata / links]
    DOM --> Extract[pagesift-extract: identity / fuzzy / forms / indicators]
    Content --> Result[Document / API response]
    Extract --> Result
    Engine --> Store[pagesift-store: cache / durable records]
```

The HTTP layer validates options before execution. Fuzzy similarity is an optional candidate mechanism: it does not override business identity. External module discovery is bounded, same-origin and opt-in, with normal crawl scope and robots checks applied to discovered URLs. New core operations are exposed through the Rust/CLI surfaces described in their own guides; the diagram does not promise every operation in every binding.

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

See [collection](incremental-collection.md), [archives](media-archive.md) and [bounded core APIs](remaining-core.md) for exact limits and failure behavior.

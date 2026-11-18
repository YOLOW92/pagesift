# Context

`Context` turns multi-format documents and persistent `Loop` sources into cross-version `KnowledgeUnit` records, dependency-governed `ContextDelta` updates, multi-strategy evidence/time-aware search, budget-constrained `ContextPack` payloads, and bounded `Action` workflows.

All interfaces share the same Rust implementation (`pagesift-graph`, `pagesift-extract`, `pagesift-store`, `pagesift-engine`, `pagesift-api`):

- **CLI:** `pagesift context <ingest|sync|search|pack|action|units|deltas|ack|learn>`
- **HTTP:** `POST /v2/context` plus sub-routes (`/v2/context/ingest`, `/sync`, `/search`, `/pack`, `/action`, `GET|POST /v2/context/units`, `GET|POST /v2/context/deltas`, `POST /v2/context/deltas/ack`, `GET|POST /v2/context/learn`)
- **MCP:** `pagesift_context`
- **Python:** `Scraper.context(...)` / `await Scraper.acontext(...)`
- **Node.js:** `await scraper.context(...)`
- **C ABI:** `pagesift_context(engine, request_json)`

`search` and `pack` support both **stateless inline execution** (passing `documents` / `--file` without configuring a `Store`) and **persistent execution** against `pagesift-store` (`ns::CONTEXT`, excluded from cache LRU eviction and scoped per authenticated tenant on HTTP).

## Core Concepts

### `KnowledgeUnit`

A `KnowledgeUnit` decouples four distinct properties so edits or layout shifts do not silently destroy or falsely merge citations:

1. **Logical identity (`unitId`):** Preserved across prefix insertions, section moves, and moderate wording rewrites when structural anchors and entity guards (`SubjectKey`, SKU/year identifiers) agree. Matching supports `boundedUot` (default: Patience/exact anchors plus entity-guarded bounded entropic Unbalanced Optimal Transport on local candidate subgraphs), `greedyDice`, and `exactAnchorOnly`.
2. **State digest (`revisionId` & `contentHash`):** Changes whenever text, claims, validity, or status change.
3. **Physical location (`location`):** Stores `docId`, `resourceKey`, `sourceUrl`, `sourceKind` (`html`, `pdf`, `office`, etc.), byte offsets (`byteStart`, `byteEnd`), optional `pageNumber`, `headingPath`, `codeLanguage`, and `tableMeta` (`caption`, `headers`, row slice bounds).
4. **Lineage & lifecycle (`lineage`, `status`):** Records `created`, `unchanged`, `relocated`, `rewritten`, `splitFrom`, `mergedFrom`, or `ambiguous` (when multiple near-identical candidates compete, `ambiguous: true` is recorded with `candidates` instead of guessing). `status` tracks `active`, `unverified`, `withdrawn`, `expired`, and `superseded`. Unreliable captures (`decode:`, `body_truncated`, `input_truncated`, `graph_truncated`) mark affected units `unverified` rather than withdrawing verified evidence.

### `ContextDelta`, `IndexVisibility`, and Checkpoints

Every persistent `ingest` or `sync` commits atomically via `pagesift-store::AtomicTx`:

- **Dependency-governed invalidation:** Derived index assets (`lexicalPosting`, `structuralAnchor`, `semanticSketch`, `tokenBudgetCache`) record `AssetDependency` stamps (`revisionId`, `locationHash`, `statusTag`, `specStamp`). Only affected units and incompatible stamps are recomputed; when changed mass or `AssetSpecStamp` (`tokenizerRev`, `sketchRev`, `unitSchemaRev`) crosses the rebuild cost threshold, maintenance switches automatically from `incremental` to `fullRebuild`.
- **`IndexVisibility`:** Reports `storeTickSeq`, `indexedTickSeq`, `lagTicks`, `deltaSeq`, and `instanceMatched` so callers can tell immediately whether the Context index is caught up with `SyncStore`. Recreating a deleted source with the same `sourceId` mints a new `instanceId` and purges stale prior Context entries (ABA protection).
- **At-least-once consumer checkpoints:** External indexers poll deltas via `deltas` (`consumerId`, optional `specFingerprint`) and commit `ack` (`instanceId`, `ackedDeltaSeq`). Duplicate or out-of-order acknowledgements are idempotent and never regress a committed sequence number.

### Multi-Strategy `Search`

`search` combines lexical BM25, exact symbol/identifier postings (`snake_case`, `camelCase`, error codes, CLI flags), heading/caption/table-header boosting, `SubjectKey` anchoring, and an optional lightweight character-trigram semantic sketch (`enableSemanticSketch`, disableable).

- **Temporal semantics:** Separates `asOf` (system observation cutoff) from `effectiveAt` (business validity window `[validFrom, validUntil]`), classifying each hit as `validAtTarget`, `expiredAtTarget`, `notYetValidAtTarget`, or `unknownValidity`.
- **Source independence & conflicts:** Groups hits by independent `originRoot` (syndicated mirrors with `originIndependent: false` do not inflate `independentSourceCount`) and co-retrieves opposing `ConflictRecord` counterparts when `includeConflicts` is true (default).
- **Six decoupled dimensions:** Every hit and report exposes `dimensions` (`relevance`, `support`, `coverage`, `freshnessValidity`, `calibratedConfidence`, `sourceIndependence`) separately alongside `rankScore`.

### Budget-Compiled `ContextPack`

`pack` compiles retrieved units into an LLM-ready `compiledContext` string and structured `sections` + `provenanceManifest` within `budgetTokens` (reserving `tokenSafetyMargin`, default `0.05`):

- **Evidence-preserving `MustKeep` rules:** Table row slices automatically re-attach their `<caption>` and GFM header row (`mustKeepTableHeaders`); units carrying negations/qualifiers (`unless`, `except`, `not`, `不含`, `除非`) and units/currencies receive hard retention priority (`mustKeepQualifiers`); opposing variants of active `ConflictRecord` entries are paired together (`mustKeepConflicts`) so single-sided conflict truncation is avoided. When the budget is too small to fit required constraints, `budgetInsufficient: true` and `omittedMustKeepReasons` are returned explicitly.
- **Selectors (`selector`):** `submodularClosure` (default: twin-greedy knapsack over term, facet, and independent-root coverage closures), `boundedDpp` (Cholesky MAP on a bounded candidate kernel), `mmrGreedy`, and `topKGreedy`.
- **Ordering (`ordering`):** `edgeSalience` (default: places highest-utility and conflict-paired sections at the prompt start and end to mitigate middle-context loss), `scoreDescending`, and `documentOrder`.

### Goal-Oriented `Action` and Drift-Guarded `Learn`

`action` supports five modes (`context`, `research`, `verify`, `watch`, `changes`):

1. Evaluates existing persisted/inline Context first (`reusedExistingContext: true` when local evidence already satisfies query coverage, freshness `maxStalenessMs`, and `minIndependentSources`, issuing zero network requests).
2. Diagnoses `CoverageDiagnosis` (`termCoverage`, `requiredFieldCoverage`, `independentSourcesObserved`, plus gated Good–Turing unseen mass and Chapman capture–recapture estimates when scope is closed and non-skewed; returns `estimable: false` with `notEstimableReason` on open-world, small-sample, head-dominated, or correlated scopes).
3. When gaps remain and `allowNetworkAcquisition: true`, evaluates sequential Value of Information (`pagesift_stat::decide::value_of_information`) against live 4D admission shadow prices (`Cpu`, `Memory`, `Browser`, `Politeness`) and executes bounded Loop syncs or candidate URL fetches within `maxAcquisitionRequests`, `allowedHosts`, `strictConstrainToUrls`, SSRF, and `robots.txt` policies, terminating with an explicit `stoppingReason` (`sufficient`, `voiBelowCost`, `budgetExhausted`, `permissionRestricted`, `noNewEvidence`, `sourceUnavailable`).

Online score calibration (`learn`) fits a monotone isotonic map (`Pool Adjacent Violators`) only on verified grounded feedback (`isHardGroundTruth: true`) and guards against distribution shift with `cusum` (Page–Hinkley, default) or `bocpd` (`BernoulliBocpd`). Detecting drift automatically freezes calibration and falls back to neutral identity scoring; operators can inspect, `--freeze`, or `--disable` learning at any time.

## Examples

### CLI

```bash
# Ingest local multi-format files into a persistent store
pagesift context ingest --store ./data/store --source-id kb \
  --file ./spec.pdf --file ./quotas.html --file ./matrix.docx

# Reconcile an existing Loop sync source into Context
pagesift context sync --store ./data/store docs

# Compile a 1024-token ContextPack with provenance citations
pagesift context pack --store ./data/store --source-id kb \
  --budget-tokens 1024 --selector submodularClosure --ordering edgeSalience \
  "Enterprise rate limits and refund exceptions"

# Verify a claim across >=2 independent sources without permitting network access
pagesift context action --store ./data/store --mode verify \
  --source-id kb --min-independent-sources 2 --json "Enterprise monthly price"
```

### HTTP (`/v2/context`)

```bash
curl -s localhost:3002/v2/context/pack \
  -H 'Authorization: Bearer sk-test' \
  -H 'Content-Type: application/json' \
  -d '{
    "sourceIds": ["kb"],
    "query": "Enterprise rate limits",
    "effectiveAt": "2026-06-01T00:00:00Z",
    "budgetTokens": 1024,
    "selector": "submodularClosure",
    "ordering": "edgeSalience"
  }'
```

### Python & Node.js

```python
import pagesift

with pagesift.Scraper(store_path="./data/store") as s:
    res = s.context(
        action="pack",
        source_ids=["kb"],
        query="Enterprise rate limits",
        budget_tokens=1024,
    )
    print(res["pack"]["compiledContext"])
```

```javascript
import { Scraper } from '@pagesift/node'

const s = new Scraper({ storePath: './data/store' })
const res = await s.context({
  action: 'pack',
  sourceIds: ['kb'],
  query: 'Enterprise rate limits',
  budgetTokens: 1024,
})
console.log(res.pack.compiledContext)
await s.close()
```

## Operational Boundaries

- **No silent network access:** `search` and `pack` never perform network I/O. `action` performs network acquisition only when `allowNetworkAcquisition` is `true` (default `false` for `context`, `verify`, and `changes`) and obeys the same SSRF, `robots.txt`, host-politeness, and `allowedHosts` policies as `scrape` and `sync`.
- **Honest statistical gating:** Coverage unseen-mass estimators require closed, multi-observation scopes; open-web queries report `estimable: false` with deterministic term/field/source coverage rather than synthetic confidence numbers.
- **Lightweight semantic sketch:** The built-in semantic sketch uses deterministic hashed character trigrams in-process (zero external embedding API or heavy neural weights); disable it via `enableSemanticSketch: false` (`--no-semantic-sketch`) when strict lexical/symbol matching is desired.

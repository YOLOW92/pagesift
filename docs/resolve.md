# Resolve

`Resolve` is PageSift's evidence-driven verification and conclusion maintenance engine. Built on top of `DataGraph` and `Context`, it turns candidate natural-language answers or explicit claims into structured, auditable, versioned `ConclusionRecord`s backed by verifiable citations, Belnap four-valued evidence status, Carneades proof standards, Minimal Cut Set vulnerability analysis, proactive counter-evidence audits, bounded gap acquisition, and incremental Live maintenance.

All interfaces share the same Rust implementation (`pagesift-graph::resolve`, `pagesift-store::resolve`, `pagesift-extract::resolve`, `pagesift-engine::resolve`, `pagesift-api::resolve`):

- **CLI:** `pagesift resolve <check|list|get|history|graph|reconcile|revalidate|events|learn|delete>`
- **HTTP:** `POST /v2/resolve` plus sub-routes (`POST /v2/resolve/check`, `GET|POST /v2/resolve/conclusions`, `GET|DELETE /v2/resolve/conclusions/{id}`, `GET /v2/resolve/conclusions/{id}/history`, `GET|POST /v2/resolve/graph`, `POST /v2/resolve/reconcile`, `POST /v2/resolve/revalidate`, `GET|POST /v2/resolve/events`, `GET|POST /v2/resolve/learn`)
- **MCP:** `pagesift_resolve`
- **Python:** `Scraper.resolve(...)` / `await Scraper.aresolve(...)`
- **Node.js:** `await scraper.resolve(...)`
- **C ABI:** `pagesift_resolve(engine, request_json)`

`check` supports both **stateless inline execution** (passing `documents` / `--file` without a `Store`) and **persistent execution** (`persist: true`) in `pagesift-store` (`ns::RESOLVE`, excluded from cache LRU eviction and strictly tenant-isolated per authenticated account on HTTP). Conclusions and inferences are never written back into `DataGraph` (`ns::GRAPH_*`) or `Context` (`ns::CONTEXT`) observation tables.

## Core Concepts

### Epistemic Stratification, Belnap States & Proof Standards

- **Epistemic layers:** Observations (`Context` `KnowledgeUnit`s), extracted claims, evaluated evidence links (`Supports`, `Refutes`, `Qualifies`, `ContextualOnly`), and derived conclusions (`ConclusionRecord`) remain strictly separated. Self-referential or transitive conclusion-to-conclusion dependency cycles (`parentConclusionIds`) are rejected atomically (`resolve_cycle_detected`).
- **Belnap four-valued evidence (`belnap`):** Tracks `hasSupport` and `hasRefutation` independently so simultaneous independent support and refutation surface as `conflicted` rather than collapsing to an average score.
- **Resolution states (`state`):** `supported`, `qualifiedInObservedScope`, `refuted`, `conflicted`, `insufficient`, `scopeIncomplete`, `causalUnidentifiable`, `unverifiable`, and `stalePendingRevalidation`.
- **Carneades proof standards (`proofStandard`):** `scintilla`, `preponderance` (default), `clearAndConvincing` (requires >= 2 independent supporting origin roots), and `strictUniversal` (requires >= 2 independent roots plus `closedObservedSet` scope for universal/superlative claims).
- **Common-cause origin collapse & Minimal Cut Sets:** Syndicated reprints or mirrors sharing the same `originRoot` are collapsed into a single independent origin event before applying proof standards or computing Reliability-Engineering Minimal Cut Sets (`cutSetAnalysis`, exposing `singlePointsOfFailure`).

### Context-Preserving Decomposition & Seven-Layer `Check`

`check` splits answers into `AtomicClaim`s while keeping conditional qualifiers (`unless`, `except`, `only if`, `除非`, `仅限`), negations, inline citations, quantifiers (`Specific`, `Universal`, `Superlative`, `NegativeExistential`), and causal markers attached, and marks subjective rhetoric `unverifiable`. Each verifiable claim passes seven deterministic layers:

1. **Citation integrity & `asOf` cutoff:** Resolves explicit inline citations (`unt_*`, URLs, `doc_*`) while hiding any observation with `observedAtMs > asOfMs` to prevent future leakage.
2. **Entity & Markdown table row/header binding:** Binds cell values to their column headers (including billing-period headers such as `Annual Price` vs `Monthly Price`) and rejects cross-entity mismatches.
3. **Quantitative, currency, unit & billing-period validation:** Distinguishes monthly vs annual billing periods, currencies (`USD`, `EUR`, `GBP`, `CNY`, `JPY`), and physical/computing units (`MiB` vs `MB`, durations, percentages).
4. **Dual-axis temporal validity:** Separates `asOf` (observation cutoff) from `effectiveAt` (business validity window `[validFrom, validUntil]` and asserted years).
5. **Polarity & qualifier omission guard:** Flags negation reversals as `refuted` and omitted conditional/exception clauses as `qualifiedInObservedScope` rather than unconditional `supported`.
6. **Causal conservatism & comparative counterexample scan:** Classifies causal assertions backed only by correlational observations as `causalUnidentifiable`; scans comparable records in the corpus for numeric counterexamples to superlative claims (e.g. `"cheapest"`).
7. **Origin-root folding & provenance construction:** Collapses mirrors by `originRoot`, evaluates proof standards, computes Minimal Cut Sets, and derives structured `unresolvedGaps` and `ScopeDependency` predicates.

An optional `verifierMode: "hybrid_assisted"` mode may invoke a user-configured `[llm]` endpoint strictly on inconclusive (`insufficient`) claims that carry no deterministic veto; any LLM verdict is accepted only after deterministic verbatim quote-grounding against the cited `KnowledgeUnit`, falling back safely to deterministic output on mismatch, timeout, or invalid JSON.

### Bounded `Gap` Resolution & Proactive `Counter` Search

- **Gap resolution (`enableGapResolution`):** Prioritizes `unresolvedGaps` by Minimal Cut Set criticality and evaluates sequential Value of Information (`pagesift_stat::decide::value_of_information`) against live 4D admission shadow prices. Network acquisition runs **only** when `allowNetworkAcquisition: true` and delegates exclusively through `ContextAction` / Loop Sync guardrails (`allowedHosts` enforced across HTTP, redirects, and browser sub-requests, `strictConstrainToUrls`, SSRF checks, `robots.txt`, `maxAcquisitionRequests`, and caller `pages_per_day` quotas).
- **Proactive counter-evidence (`enableCounterSearch`):** Plans targeted probes (`officialErrataOrUpdate`, `exceptionOrQualifier`, `directContradiction`, `comparativeDomainScan`) weighted by single-point-of-failure premises, scans local (and optionally budgeted network) units, and attaches a `CounterSearchAudit` recording checked roots, blocked targets, search failures, competing hypotheses, and an explicit coverage statement noting that absence of found counter-evidence is not proof.

### Persistent `Live` Maintenance & Conservative `Learn`

- **Immutable history & point-in-time replay:** Every committed conclusion update increments `version`, appends `CorrectionRecord`s on state transitions, preserves immutable `ConclusionRevision` snapshots (`GET /v2/resolve/conclusions/{id}/history` and `?asOf=`), and emits monotonic `ResolveEvent` records (`GET /v2/resolve/events`).
- **Incremental `reconcile` & `revalidate`:** Consumes `ContextDelta` streams via ABA-safe `(instanceId, deltaSeq)` checkpoints (`list_deltas_checked`):
  - **Early Cutoff:** `RelocatedOnly` and `Reconfirmed` transitions do not falsely invalidate conclusions.
  - **Observation caution:** `ObservationLost` transitions mark dependent conclusions `stalePendingRevalidation` (`staleReason: "observation_lost_pending_confirmation"`) and never mark them `refuted`.
  - **Scope / negative dependencies:** Newly appearing or modified units are matched against `ScopeDependency` predicates (`rscp` index) so new competitors or incident disclosures invalidate affected universal, superlative, and negative-existential conclusions even when previously cited pages did not change.
  - **Conservative fallback:** Delta retention gaps, sequence resets, or ABA source-instance replacement (`InstanceSuperseded`) automatically invalidate all conclusions linked to that source for full revalidation.
- **Conservative `Learn` (`GET|POST /v2/resolve/learn`):** Fits domain- and claim-kind-stratified isotonic calibration (`Pool Adjacent Violators`) strictly on externally verified hard labels (`isHardGroundTruth: true`), ignores weak signals, returns `calibratedProbability: null` while cold or uncalibrated, freezes and resets to uncalibrated mode on Page–Hinkley CUSUM distribution drift or `verifierVersion` changes, and keeps `GET /v2/resolve/learn` strictly read-only.

## Examples

### CLI

```bash
# Verify an answer against local files and persist the versioned conclusion
pagesift resolve check --store ./data/store --file ./pricing.md \
  --answer "Plan Pro costs $99/yr." --persist --json

# Reconcile Context updates and incrementally revalidate only impacted conclusions
pagesift resolve reconcile --store ./data/store --source-id pricing --json
pagesift resolve revalidate --store ./data/store --source-id pricing --mode incremental --json

# Inspect dependency graph, cut sets, and point-in-time revision history
pagesift resolve graph --store ./data/store --json
pagesift resolve history --store ./data/store <conclusion_id> --json
```

### HTTP (`/v2/resolve`)

```bash
curl -s localhost:3002/v2/resolve/check \
  -H 'Authorization: Bearer sk-test' \
  -H 'Content-Type: application/json' \
  -d '{
    "sourceIds": ["pricing"],
    "answer": "Plan Pro costs $99/yr.",
    "proofStandard": "clear_and_convincing",
    "enableCounterSearch": true,
    "persist": true
  }'
```

### Python & Node.js

```python
import pagesift

with pagesift.Scraper(store_path="./data/store") as s:
    res = s.resolve(
        action="check",
        source_id="pricing",
        answer="Plan Pro costs $99/yr.",
        persist=True,
    )
    print(res["outcome"]["conclusion"]["state"])
```

```javascript
import { Scraper } from '@pagesift/node'

const s = new Scraper({ storePath: './data/store' })
const res = await s.resolve({
  action: 'check',
  sourceId: 'pricing',
  answer: 'Plan Pro costs $99/yr.',
  persist: true,
})
console.log(res.outcome.conclusion.state)
```

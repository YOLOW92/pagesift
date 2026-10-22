# Public comparison evidence

The `2.0.0/` snapshot contains **frozen pre-release measurements from 2026-10-22**, not a new benchmark run of the version-bumped release binary. Scores are diagnostic and scope-specific. SVGs in `docs/assets/` are generated from this snapshot with `python3 scripts/release_visuals.py`.

| Evidence | Contents |
|---|---|
| [Manifest](2.0.0/manifest.json) | SHA-256 and byte sizes of the copied records |
| [Summary](2.0.0/summary.json) | All six references, known losses, partial capabilities and exclusions |
| [Component protocol](2.0.0/content/protocol.json) / [input manifest](2.0.0/content/manifest.jsonl) | Fixed 545-page dataset revision, preprocessing, budgets, input hashes |
| [Pagesift component](2.0.0/content/pagesift-final-component/scores.json) / [Essence component](2.0.0/content/essence-component/scores.json) | Per-page F1 and execution failures |
| [Component pairing](2.0.0/content/paired.json) | Wins, ties, losses and host-cluster bootstrap |
| [Service protocol](2.0.0/service-isolated/protocol.json) | Five-container Firecrawl image digests, replay context, fault isolation and resource split |
| [Service summary](2.0.0/service-isolated/scores-summary.json) / [pairing](2.0.0/service-isolated/paired.json) | Full 545-page scores, failure denominator and losses |
| [Pagesift service](2.0.0/service-isolated/pagesift/scores.json) / [Firecrawl service](2.0.0/service-isolated/firecrawl/scores.json) | Per-page replay scores |
| [Recovery record](2.0.0/service-isolated/recoveries.json) | Failed page retained; reference stack restarted before next page |
| [Control protocol](2.0.0/tasks-verified/protocol.json) / [results](2.0.0/tasks-verified/summary.json) | Navigation, forms, fake-key indicators, relocation, originals and stream decoding |
| [Source identity](2.0.0/source.json) / [reference identity](2.0.0/reference-environment.json) | Measured candidate source/binary hashes and pinned reference environments |
| [Dependencies](2.0.0/dependencies.json) | Prior core round dependency comparison, before the 2.0 workspace version bump |
| [Python SDK](2.0.0/sdk-verified/python.json) / [JS SDK](2.0.0/sdk-verified/js.json) | Official Firecrawl SDK compatibility: 17 checks |
| [Article regression](2.0.0/aeb-summary.json) / [archive regression](2.0.0/media-verified/summary.json) | 181 byte-identical article outputs; resume, crash and locking checks |

## Reproduce and inspect history

[Harness instructions](../README.md), [version lock](../tools.lock.json), pinned runtime locks and all benchmark drivers are committed. Obtain the dataset from the fixed URL in the component protocol; website bodies and gold text are not bundled. Run commands with a new result directory and preserve failures. The runner caps aggregate work at 8 logical CPUs / 8 GiB and reserves disk/memory.

`bench/six/results/` and `bench/data/` are deliberately ignored: raw HTML/Markdown, downloaded upstream sources, logs, build caches and binaries remain local. Historical reports reference those paths as local provenance, not guaranteed public links. Earlier failed attempts remain locally available; their existence, fault-isolation changes and scoring corrections are disclosed in [SEVEN-CORE.md](../SEVEN-CORE.md). Public records may contain original local paths as provenance; they do not imply those files are included in the repository.

Snapshot fields such as `no_commit_or_push`, base HEAD, cleanup free-space measurements and test counts describe the frozen measurement round. They do not describe the later release state. Release validation and distribution status are recorded separately in GitHub Actions and the release notes.

The [measured offline adapter lock](../locks/offline-measured-Cargo.lock) preserves the original 0.12.1 candidate environment. The active adapter lock follows the 2.0 workspace paths (including extract's direct `url` dependency); third-party locked versions are unchanged. This tooling synchronization does not rerun or relabel frozen scores.

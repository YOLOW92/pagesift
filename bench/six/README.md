# Six-project comparison — executable first round

> 2.0 public snapshot: [protocols, per-page scores and hashes](evidence/README.md). Historical raw `results/` directories remain local and are ignored. See [release guide](../../docs/releases/2.0.0.md).

This extends the existing `bench/compare`, AEB, local-origin, SDK and local-CI workflow.
Read [the workflow and source investigation](../../docs/comparison-plan-2026-10-22.md).
This is an initial diagnostic round, **not a six-product speed ranking or final release acceptance**.

## Resource limits

`run.py` starts each command in a systemd user service. All such services share
`pagesift-six.slice`: **eight logical CPU IDs (0–7), 800% CPU quota, 8 GiB RAM,
zero swap**. Build jobs default to eight. Single-core measurements must explicitly
pass `--cpus 2 --cpu-quota 100` and set underlying pools to one thread.
The runner reserves 10 GiB disk, checks system memory, and stops only its own service
on resource pressure or its deadline. It saves commands, hashes, patch, limits,
stdout/stderr and failure status. Cgroup memory includes cache and is **not PSS**;
the sampled peak is a lower bound; systemd's terminal summary is preserved as well.
For a multi-service Docker experiment, add explicit container accounting and limits
before running it; this runner alone cannot bound Docker-daemon descendants.

The initial preparation used the more conservative 2-CPU/3-GiB limits before the
user explicitly raised the allowance. Per-run records retain their actual limits.
No user applications are stopped and no system-wide cache is dropped.

## Reproduce

Requires Linux with cgroup v2, systemd user services, Python, Node/npm and Rust.
Recorded environment: Python 3.14, Node 24.21, Rust stable and 1.88, existing system
Chrome. Dependencies live under ignored `bench/data/six`; no browser is downloaded.

```bash
python3 bench/six/run.py --id sources --timeout 600 \
  --input bench/six/tools.lock.json -- python3 bench/six/prepare_tools.py
python3 bench/six/run.py --id bootstrap --timeout 1200 -- python3 bench/six/bootstrap.py

# bootstrap installs the minimal parser/crawler environment. Browser and SDK tests
# additionally use the pinned packages captured in locks/package*.json.
cp bench/six/locks/package*.json bench/data/six/node/
(cd bench/data/six/node && PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci --no-audit --no-fund)
bench/data/six/venv/bin/python -m pip install --no-cache-dir \
  bench/data/six/sources/firecrawl/apps/python-sdk

python3 bench/six/run.py --id components --timeout 1200 -- python3 bench/six/build_tools.py
python3 bench/six/run.py --id main-component --timeout 1200 -- python3 bench/six/build_tools.py --essence main
python3 bench/six/prepare.py

# Use a NEW result directory every time: evidence is not overwritten or resumed
# merely because line counts happen to match.
python3 bench/six/run.py --id offline --timeout 600 --cpus 2 --cpu-quota 100 \
  --env RAYON_NUM_THREADS=1 --env TOKIO_WORKER_THREADS=1 \
  --input bench/data/six/fixtures.jsonl --input bench/data/six/aeb.jsonl \
  -- python3 bench/six/baseline.py bench/six/results/my-offline-run
python3 bench/six/score.py bench/six/results/my-offline-run
python3 bench/six/run.py --id discovery --timeout 240 \
  -- python3 bench/six/discovery.py bench/six/results/my-discovery-run
python3 bench/six/run.py --id adaptive --timeout 300 \
  -- python3 bench/six/adaptive.py bench/six/results/my-adaptive-run target/release/pagesift
python3 bench/six/run.py --id relocation --timeout 90 \
  -- bench/data/six/venv/bin/python bench/six/relocation.py bench/six/results/my-relocation-run
python3 bench/six/run.py --id archive --timeout 90 \
  -- bench/data/six/venv/bin/python bench/six/gallery_archive.py bench/six/results/my-archive-run
python3 bench/six/run.py --id sdk --timeout 300 \
  -- bench/data/six/venv/bin/python bench/six/sdk.py bench/six/results/my-sdk-run target/release/pagesift
```

`prepare.py` requires the existing downloaded AEB repository under
`bench/data/fresh/data/aeb`. It decompresses the **unchanged 181-page corpus**;
it does not choose documents using pagesift's output. The 12 image fixtures are
transparent synthetic cases, not a representative web sample. Original raw and
rendered snapshot corpora were retained; byte-identical older snapshot files were
hard-linked during cleanup. Treat those frozen inputs as read-only and regenerate
new corpora into a new directory rather than truncating existing snapshot files.

`locks/` records the actual source/binary hashes, Python package versions and npm
lockfile after browser/SDK runtime installation. `bootstrap.py` alone is a fresh
environment resolver; use the recorded locks for a frozen replication. Python
freeze entries with local file URLs refer to the three locked source checkouts
(Scrapling, gallery-dl and the Firecrawl SDK); remap the checkout root when replicating
elsewhere. Essence's v0.3.0 tag reports Cargo package version 0.2.0; identify it by
**tag + SHA**, not by silently relabeling the package.

## Scope and interpretation

| Project | Executed | Important boundary |
|---|---|---|
| Firecrawl v2.11.0 | Full five-service self-hosted API on 193 frozen replay pages; Go component; official Python 4.30.1 and JS 4.28.1 SDKs against pagesift | Static replay with CSP; not cloud or live-web quality. Component scores are separate. No speed ranking |
| Crawlee 3.18.2 | CheerioCrawler + conventional enqueueLinks; AdaptivePlaywrightCrawler with system Chrome | Request handlers are disclosed; no extra Markdown converter; no population-level throughput claim |
| Scrapling 0.5 | Native parser tests; exact selectors and default-threshold adaptive relocation | Raw `img::attr(src)` is only a selector baseline, not a smart image-extraction feature |
| Katana 1.8.0 | Native same-host HTTP discovery with JS parsing | Native mode violated robots once; explicit fixture-path exclusion retained 6/7 coverage with zero violations. This is not general robots support |
| Essence v0.3.0 + main SHA | Unmodified Markdown/image modules on fixed HTML, with thin error-type shim | Excludes fetch, browser fallback and service validation; dependencies are in the adapter's Cargo.lock |
| gallery-dl 1.32.17 | Its real GenericExtractor on fixed HTML and its real DownloadArchive across reopen | Not a full site-extractor coverage or downloader benchmark; site-item identity differs from URL cache identity |

Image scores distinguish the image field and Markdown: the scorer checks whether
the preferred gold URL appears, not whether every lower-resolution variant is
wrong. Missing output capabilities are N/A in the report, not zero-quality claims.
Adaptive fixtures demonstrate a correctness issue in a controlled page family;
they cannot establish an overall rendering success rate. Relocation scoring v2
normalizes whitespace consistently on both sides: differing text-node separators
must not become a fake selector failure. The original v1 diagnostics are retained
but superseded. Failed setup/compilation/CI attempts are also retained.

The first CI run exposed a receiver/ACK race in the webhook test's metrics check.
The corrected test waits, with a bound, for the sender's delivered counter; it still
requires the same ordered events, signatures, retry count and delivery count.

## Regression and next stages

```bash
python3 -m unittest discover -s bench/six/tests -v
python3 bench/six/run.py --id ci --timeout 1800 \
  --env CARGO_TARGET_DIR=/home/yolow92/pagesift/bench/data/six/rust-target \
  --env RUST_TEST_THREADS=4 -- bash scripts/ci.sh
```

Remap the absolute build directory on another machine. For exploratory rebuilds,
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0` reduces
disk use; the archived CI records show the actual flags used for this round.
Large debug artifacts can be removed **after** checks finish; preserve baseline
and candidate binaries plus their hashes. Never delete an active target directory.

Remaining release acceptance work: general politeness validation,
broader real-site discovery and held-out rendering families, calibrated relocation
negatives before adding a fuzzy extractor, site-specific archive workloads,
independent new-site
release runs. These are not claimed complete by the first-round report.

## Full Firecrawl replay

`firecrawl_stack.py` prepares a private five-service Compose stack from immutable
image digests. `firecrawl_full.py` verifies origin identity and HTTP 200 before
scoring, saves raw responses, samples all five containers, and removes its own
containers and volumes in a finally block. See `locks/firecrawl-runtime.json`.
The API, browser, database, Redis and RabbitMQ budgets total 6784 MiB and 7.2 CPUs;
the runner uses at most 1024 MiB and 0.5 CPU during this experiment. Docker daemon
children are bounded explicitly because the user-service slice does not contain them.
`results/firecrawl-full-domain` is the valid replay; earlier invalid attempts are
excluded by `results/audit-notes.json`. `diff.py` preserves all per-page comparisons
and a site-cluster paired bootstrap rather than hiding losses behind an average.

# Contributing to pagesift

Thanks for helping. Bug reports, benchmark results from your own sites, and pull requests are all welcome.

## Before you start

- **Ground rules.** pagesift scrapes politely: it respects robots.txt, rate-limits per host, and never bypasses anti-bot protection. Pull requests that add stealth, browser or TLS fingerprint impersonation, CAPTCHA solving or proxy rotation will be closed.
- **CLA.** pagesift is dual-licensed (AGPL-3.0 and commercial, see [COMMERCIAL.md](COMMERCIAL.md)), so every contributor signs the [Contributor License Agreement](CLA.md) once. A bot asks you on your first pull request.
- **Dependencies.** Only permissively licensed dependencies (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, CC0). `cargo deny check` enforces this.
- **Big changes.** Open an issue first so we can agree on the approach.

## Development

Requirements: Rust 1.88 or newer. For browser rendering, one of Lightpanda, moli or Chrome/Chromium on `PATH` (`pagesift doctor` shows what it found).

```bash
cargo build --release
cargo test --workspace
scripts/ci.sh            # the full local check: fmt, clippy, tests, MSRV, cargo deny, perf and crawl gates
```

`scripts/ci.sh` must pass before a pull request is merged. Size parallel builds with `CARGO_BUILD_JOBS=$(scripts/jobs.sh)` on machines with little memory.

Native Node tests use `npm test` in `crates/pagesift-node`. Test files run sequentially so separate Rust worker pools do not compete with event-loop latency measurements; each concurrency test still exercises its own parallel work.

### Performance

Performance is a feature. Changes on the hot paths (HTML parsing, extraction, the HTTP client, crawling) should come with numbers:

- `cargo run --release -p pagesift-content --example perf_gate` and `cargo run --release -p pagesift --example crawl_gate` are the regression gates run by `scripts/ci.sh`.
- `bench/` holds the comparisons with other tools; see [docs/benchmarks.md](docs/benchmarks.md).

On a shared machine, compare instruction counts (`perf stat -e instructions`) rather than wall time.

### Style

- `cargo fmt`, and no clippy warnings (`-D warnings`).
- Comments and commit messages may be in English or Chinese. Commit messages: a short imperative subject line, then what changed and why.
- `unsafe` is forbidden outside `crates/pagesift-sys`.

## Reporting security issues

Please don't open public issues for vulnerabilities, especially SSRF bypasses. See [SECURITY.md](SECURITY.md).

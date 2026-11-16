# Contributing to pagesift

Thanks for helping. Bug reports, benchmark results from your own sites, and pull requests are all welcome.

## Before you start

- **Ground rules.** pagesift scrapes politely: it respects `robots.txt` by default, rate-limits per site across all egress IPs, and never solves CAPTCHAs. Browser fingerprint mode is off by default and only an operator can turn it on; with it on, sites see Chrome and not pagesift, and whether that complies with a site's terms is the operator's responsibility. Pull requests that add CAPTCHA solving, or that repeat requests to get past challenge pages, will be closed. Egress switching is allowed only for IP-reputation blocks: it must never raise the request rate to a site, and rate-limit responses (429, 503 with `Retry-After`) must always be backed off, never routed around.
- **CLA.** pagesift uses a [Contributor License Agreement](CLA.md) so contributions can be licensed under Apache-2.0 and enterprise terms. A bot asks you on your first pull request.
- **Dependencies.** Only permissively licensed dependencies (`MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Zlib`, `Unicode-3.0`, `Unlicense`, `CC0-1.0`). `cargo deny --config .github/deny.toml check` enforces this.
- **Big changes.** Open an issue first so we can agree on the approach.

## Development

Requirements: **Rust 1.89 or newer** (the `rust-version` in [Cargo.toml](../Cargo.toml)). For browser rendering and `pagesift-cdp` integration tests, install one of `Lightpanda`, `moli`, or `Chrome`/`Chromium` on `PATH` or in a standard location (`pagesift doctor` probes what is installed and starts each browser engine to verify it).

### Profiles and everyday test commands

[Cargo.toml](../Cargo.toml) defines three profiles:

- `--profile quick`: incremental debug builds with light optimization (`opt-level = 1` for workspace crates, `opt-level = 2` for dependencies). **Use this for daily development and tests.**
- `--profile release-fast`: ThinLTO with 16 codegen units — use when testing optimized behaviour or running rough benchmarks.
- `--release`: Fat LTO (`codegen-units = 1`) — reserved for release binaries and final benchmark/regression gates; avoid running workspace-wide tests under `--release`.

```bash
# Fast formatting, lints, and tests (skipping full browser-render suites when no browser is installed)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked --profile quick -- -D warnings
cargo test --workspace --locked --profile quick --exclude pagesift-cdp
cargo test -p pagesift-cdp --locked --profile quick --lib --test protocol --test browser --test bench
```

If running unoptimized `dev`-profile tests (`cargo test` without `--profile quick`), set `RUST_MIN_STACK=8388608` so async crawl chains do not overflow the default 2 MiB test thread stack.

Size parallel builds with `CARGO_BUILD_JOBS=$(scripts/jobs.sh)` on machines with limited memory.

### Full local CI & native bindings

`scripts/ci.sh` is the maintainer Linux gate (it requires Linux, the `aarch64-apple-darwin` rustup target for macOS type-checking, and optional local Python/Node environments unless `--fast` is passed):

```bash
scripts/ci.sh --fast     # release sync, wall-clock/workflow checks, fmt, clippy, workspace tests, macOS check
scripts/ci.sh            # full gate: adds Node & Python bindings, MSRV check, cargo deny, and perf/crawl/focus gates
```

- **Node binding (`crates/pagesift-node`)**: build the native `.node` addon first (`npm run build:debug` or `npm run build`), then run `npm test` (`node --test --test-concurrency=1`). Test files run sequentially so separate Rust worker pools do not compete with event-loop latency measurements; each concurrency test still exercises its own parallel work.
- **Python binding (`crates/pagesift-py`)**: build in your virtualenv with `maturin develop --profile quick --locked` inside `crates/pagesift-py`, then run `pytest crates/pagesift-py/tests`.
- **Other bindings**: C/FFI and language wrappers live in `crates/pagesift-ffi` and `bindings/`.

### Performance

Performance is a feature. Changes on the hot paths (HTML parsing, extraction, the HTTP client, crawling, PDF/Office parsing, OCR/DataGraph) should include measurements:

- `cargo run --release --locked -p pagesift-content --example perf_gate`, `cargo run --release --locked -p pagesift --example crawl_gate`, and `cargo run --release --locked -p pagesift --example focus_gate` (+ `--auto`) are the regression gates run by `scripts/ci.sh`.
- `benchmarks/` holds comparisons with other tools; see [docs/benchmarks.md](../docs/benchmarks.md).

On a shared machine, compare instruction counts (`perf stat -e instructions`) rather than wall time.

### Style & checks

- `cargo fmt`, zero clippy warnings (`-D warnings`), and zero broken Markdown links (`scripts/check_links.sh`).
- `scripts/check_file_length.sh`: non-test `crates/*/src/**/*.rs` files must stay within single-responsibility length limits (<= 1000 lines unless explicitly justified).
- `scripts/check_wall_clock.py` and `scripts/check_workflows.py`: enforce deterministic test timing rules and workflow hygiene.
- When editing `README.md` or translated READMEs in `docs/`, run `python3 -I scripts/check_readme.py`.
- Pure-Rust application and parser crates enforce `#![forbid(unsafe_code)]`. `unsafe` is restricted strictly to FFI, allocator, GPU/SIMD inference, and test-harness crates (`pagesift-sys`, `pagesift-ffi`, `pagesift-gpu`, `pagesift-infer`, `pagesift-testkit`), generated binding glue (`pagesift-py`, `pagesift-node`), and vendored TLS (`pagesift-rustls`). Any new `unsafe` block requires a clear `// SAFETY:` invariant comment.
- Comments may be in English or Chinese. For pull requests, write a clear imperative summary explaining what changed and why.

## Reporting security issues

Please don't open public issues for vulnerabilities (SSRF bypasses, multi-tenant/DataGraph leaks, parser/memory-safety DoS, or sandbox escapes). See [SECURITY.md](SECURITY.md).

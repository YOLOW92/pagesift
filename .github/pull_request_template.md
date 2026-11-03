## What and why

<!-- What this changes and the problem it solves. Link the issue if there is one. -->

## How it was tested

<!-- Tests added or run. For hot-path changes, before/after numbers (perf_gate, crawl_gate or benchmarks/). -->

## Checklist

- [ ] `scripts/ci.sh` passes
- [ ] No new copyleft dependencies (`cargo deny --config .github/deny.toml check`)
- [ ] Nothing that bypasses robots.txt or rate limits, solves CAPTCHAs, retries past challenge pages, or turns browser fingerprint mode on by default
- [ ] I have signed the [CLA](CLA.md) (the bot asks on your first PR)

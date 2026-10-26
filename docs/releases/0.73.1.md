# PageSift 0.73.1 Release Notes: A Shared MCP Service That Reports Its Own Mistakes

> **Release Version**: `v0.73.1`
> **Release Date**: 2026-10-26
> **License**: AGPL-3.0-only (Commercial licensing available)

---

## Overview

0.73.1 makes `pagesift mcp http` fit to run as one long-lived service shared by every AI agent on
a machine, and turns the traffic it serves into bug reports:

- **10× faster MCP calls over HTTP.** Every call on a kept-alive connection waited about 40 ms
  for a TCP delayed ACK; that wait is gone.
- **Audit log.** One JSON line per fetch, HTML snapshots of suspicious results, and a
  `pagesift_report_issue` tool for agents to record wrong results.
- **Cache and warnings in MCP.** `mcp http` uses the configured cache, and the scrape tool tells
  agents when its result may be incomplete.
- **Clean shutdown.** `SIGTERM` closes the browser pool before the process exits.

Setup guide: [Running pagesift as a shared MCP service](../mcp-service.md).

---

## 40 ms per MCP call

The MCP server writes a response as headers followed by a chunked body. With Nagle's algorithm
on, the second small write waited for the client's delayed ACK, so every call on a kept-alive
connection — which is how agents talk to an MCP server — took at least 40 ms however fast the
page was. The MCP and API servers now set `TCP_NODELAY` on every connection.

`pagesift_scrape` over MCP HTTP on a local site, alternating 0.73.0 and 0.73.1, three rounds
(i7-10700, 8 cores / 16 threads):

| Concurrent clients | 0.73.0 req/s | 0.73.1 req/s | 0.73.0 p50 | 0.73.1 p50 |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 26 | 256–266 | 41.0 ms | 3.0–3.1 ms |
| 4 | 104–105 | 907–1145 | 41.0 ms | 2.9–3.5 ms |
| 16 | 415 | 2065–2343 | 41.0 ms | 5.5–6.1 ms |

Requests per second: higher is better; latency: lower is better. CPU time per request (lower is
better, median of four rounds): MCP 3,805 → 3,584 µs, `serve` 3,673 → 3,677 µs, so `serve`
throughput is unchanged; its run-to-run spread on this machine is about ±15%.

---

## Audit log

`--audit-log <path>` on any command, or in `pagesift.toml`:

```toml
[audit]
path = "/home/me/.local/state/pagesift/audit.jsonl"
# max_mb = 64            rotate to <path>.1 past this size
# snapshots = true       keep HTML of suspicious results
# snapshot_dir = "…"     default: snapshots/ next to the log
# snapshot_max_kb = 2048
# snapshots_per_day = 200
# redact = true          strip URL credentials and secret-looking query values
```

Each fetch — single scrapes, batch pages, crawl pages, MCP calls — appends one line with the
source (`mcp-http`, `serve`, `scrape`…), the MCP client's name, URL, final URL, engine and
browser engine, status, content type, cache state, time, extracted characters, HTML size,
warnings, error code and the anti-bot rule that matched. Page text is not logged.

A result is marked `suspicious` when it carries a content warning (`empty_content`,
`thin_content`, `render_unavailable`, `render_skipped_*`), when an HTML page returns 200 with
under 200 characters of text, or when the fetch fails for a reason that is not the caller's
(robots, invalid URLs, private addresses and unsupported options are not counted). Suspicious
results get a snapshot of the HTML pagesift worked from — the rendered DOM when a browser was
used — so the problem can be replayed after the page has changed.

With an audit log configured, the MCP server also offers `pagesift_report_issue` (URL, expected,
actual, optional parameters and note). Reports go into the same log as `kind: "report"`; nothing
leaves the machine.

Cost: CPU per request with the audit log and snapshots on was within run-to-run noise of the log
being off (six alternating rounds at 16 clients: medians 3,735 and 3,886 µs).

---

## MCP changes

- **Cache.** `pagesift mcp http` opens the result cache from `[cache]` (`enabled`, `dir`,
  `max_mb`), as `serve` does. `mcp stdio` still runs without it: each client starts its own
  process.
- **One process per data directory.** The store now takes a lock file. If another process holds
  the directory, `mcp http` prints a warning and runs without the cache; `serve` reports the
  conflict.
- **Warnings in text.** `pagesift_scrape` adds a `Warnings: thin_content, …` line under the
  title and URL. The full warnings stay in the structured result.
- **Shutdown.** On `SIGTERM` the HTTP transport stops accepting requests, then the browser pool
  is closed.
- **Library API.** `pagesift::mcp::run_server` is unchanged; `run_server_until` takes a shutdown
  future.

---

## Upgrade notes

- The minimum supported Rust version is 1.89 (standard-library file locks).
- Running `serve` and `mcp http` against the same data directory is no longer possible at the
  same time: give them different `[cache] dir` values, or let `mcp http` run without the cache.
- No configuration keys were renamed or removed; `[audit]` is new.

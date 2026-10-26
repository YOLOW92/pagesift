# Running pagesift as a shared MCP service

One long-running `pagesift mcp http` process can serve every AI coding agent on a machine
(Claude Code, Cursor, …). Compared with each agent starting its own `pagesift mcp` over stdio, a
shared service keeps one browser pool, one cache and one place to look when a page comes back
wrong.

## Start the service

`pagesift mcp http` listens on `127.0.0.1` only (default port 3003) and checks `Host` and
`Origin` on every request, so other machines and web pages cannot reach it.

A systemd user unit, `~/.config/systemd/user/pagesift-mcp.service`:

```ini
[Unit]
Description=pagesift MCP server

[Service]
ExecStart=%h/.cargo/bin/pagesift mcp http --port 3003
Restart=always
# Safety nets for a process that runs for weeks: a memory ceiling for pagesift and its
# browsers, and a daily restart.
MemoryMax=3G
RuntimeMaxSec=1d
# Keep the default KillMode=control-group so browser processes stop with the service.

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now pagesift-mcp
```

Register it once for all Claude Code projects:

```bash
claude mcp add --scope user --transport http pagesift http://127.0.0.1:3003/mcp
```

On `SIGTERM` (`systemctl stop`, the daily restart) the service stops accepting requests and closes
its browsers before exiting.

## Configuration

The service reads `~/.config/pagesift/pagesift.toml` like every other command:

```toml
[cache]
max_mb = 2048          # without a cap the cache grows for as long as the service runs

[policy]
interval_ms = 1000     # per-host politeness, shared by every client

[browser]
max_browsers = 2
min_available_memory_mb = 1500

[audit]
path = "/home/me/.local/state/pagesift/audit.jsonl"
```

- **Cache.** `mcp http` uses the result cache from `[cache]`, like `serve`. `mcp stdio` does not:
  every client starts its own process, and the store cannot be shared between processes. A data
  directory can be open in one process at a time; if `serve` already holds it, `mcp http` prints
  a warning and runs without the cache.
- **Limits are shared.** All clients share the global concurrency and the per-host rate limit. A
  large `pagesift_crawl` started by one agent makes other agents wait for the same host.
- **robots.txt is honoured** for single-page scrapes too. Sites that disallow crawlers (Reddit,
  Zhihu, IMDb, …) return `ROBOTS_DISALLOWED`; the agent should fall back to another fetch tool.
- **Crawl jobs** started through MCP live in memory and are lost on restart.

## Audit log

With `[audit] path` (or `--audit-log <path>` on any command), every fetch appends one JSON line:

```json
{"ts":"2026-10-26T12:04:30.505Z","kind":"scrape","version":"0.73.1","source":"mcp-http",
 "client":"claude-code","url":"https://example.com/a","engineRequested":"auto","engine":"browser",
 "browserEngine":"chrome","status":200,"contentType":"text/html","cache":"miss","ms":812.4,
 "textChars":5996,"htmlBytes":436086,"warnings":["thin_content: …"],"suspicious":"thin_content",
 "snapshot":"/home/me/.local/state/pagesift/snapshots/2026-10-26/1792…-3f2a9c0d1e7b.html"}
```

| Field | Meaning |
| :--- | :--- |
| `kind` | `scrape`, `crawl` (a page fetched by a crawl) or `report` |
| `source` | how pagesift was run: `mcp-http`, `mcp-stdio`, `serve`, `scrape`, `crawl`… |
| `client` | the MCP client's self-reported name (not verified) |
| `engine`, `browserEngine` | what actually produced the page |
| `textChars`, `htmlBytes` | characters of extracted text, bytes of the HTML it came from |
| `warnings` | the document's warnings (`thin_content`, `render_unavailable`, …) |
| `errorCode`, `error`, `barrierRule` | for failed fetches, including which anti-bot rule matched |
| `suspicious` | why this result may have lost content: a content warning, under 200 characters from an HTML page, or an error that is not the caller's (robots, invalid URL, private address and unsupported options are not counted) |
| `snapshot` | the HTML pagesift worked from, saved for suspicious results so the problem can be replayed after the page changes |

- Only metadata is logged; page text is not.
- URLs lose user names and passwords, and query values whose names look like credentials
  (`token`, `key`, `signature`, `session`, `code`, …) become `REDACTED`. Set `redact = false` to
  keep them.
- The log rotates to `<path>.1` past `max_mb` (default 64), so it never takes more than twice
  that.
- Snapshots go to `snapshots/` next to the log (`snapshot_dir` to change it), at most
  `snapshots_per_day` (default 200) of at most `snapshot_max_kb` (default 2048) each;
  `snapshots = false` turns them off.

### Reporting wrong results

When an audit log is configured, the MCP server also offers `pagesift_report_issue`. An agent
that notices a wrong result (missing body text, empty table, lost headings) calls it with the
URL, what it expected and what it got; the report goes into the same log with `kind: "report"`.
Nothing is sent anywhere.

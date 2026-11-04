# Monitors (`/v2/monitor`)

Firecrawl's monitor API shape: `POST /v2/monitor`, `GET /v2/monitor`, `GET` / `DELETE
/v2/monitor/{id}`, `GET /v2/monitor/{id}/checks`, `GET /v2/monitor/{id}/checks/{checkId}`
(`/v2/monitors` is an alias). pagesift adds `POST /v2/monitor/{id}/run` to check every URL now.

```json
{
  "name": "Pricing",
  "targets": [{"type": "scrape", "urls": ["https://example.com/pricing"],
               "scrapeOptions": {"formats": ["markdown", {"type": "extract", "schema": {...}}]}}],
  "webhook": {"url": "https://example.com/hook", "events": ["monitor.page", "monitor.check.completed"]}
}
```

| Field | pagesift |
|---|---|
| `targets` | 1–50; `scrape` (`urls`, `scrapeOptions`) or `crawl` (`url`, `crawlOptions`: each check crawls and reports `new`, `changed`, `removed` pages). `search` → `UNSUPPORTED_OPTION` |
| `schedule` | `cron` (evenly spaced forms) or `text` (`every N minutes`, `hourly`, `every N hours`, `daily`, `weekly`): checks at that period, at least 5 minutes. Without a schedule, or with the pagesift fields `minIntervalMs` / `maxIntervalMs` (milliseconds), the interval adapts (below). Only the period of a schedule is applied, counted from creation: a time of day, weekday or non-UTC `timezone` is not, and the response carries a `monitor_time_ignored` / `monitor_timezone_ignored` warning |
| `webhook` | `monitor.page` for every page that is not `same`, `monitor.check.completed` after each check; payloads as in Firecrawl, signed like job webhooks |
| `notify` | pagesift: `substantive` (default) or `any` |
| `goal` | stored; changes are not judged against it (`monitor_goal_not_judged` warning). `judgeEnabled: true`, `notification` (email, Slack) → `UNSUPPORTED_OPTION` |
| `retentionDays` | 1–365, default 30; at most the latest 1000 checks are kept per monitor |

Page statuses: `same`, `new`, `changed`, `removed` (404/410, or no longer found by a crawl that ran
to completion; reported once, later 404s are `same`), `error` (fetch failed, including other 4xx/5xx
responses; the saved content is kept and the failure does not count as "unchanged"). A check is
`completed`, `partial` (some pages failed) or `failed` (all did, or the crawl itself failed). Every
fetched page counts against the owner's daily page quota; with the quota used up pages are not
fetched and report `QUOTA_EXCEEDED`. A change counts as `changed` when the block diff finds a substantive change: dates, relative
times, counters and advertisement slots (blocks starting with a disclosure label such as "Sponsored",
"Advertisement", "广告") are noise, prices and other text are not; with `notify: "any"` noise counts
too, otherwise noise-only pages report `same` with `noiseOnly: true`. Changed pages carry `diff.text`
(unified diff), `diff.blocks`, and `fieldChanges` (`{item, field, before, after}`) when the scrape
options include an `extract` schema. Scrape targets send `If-None-Match` / `If-Modified-Since`; a 304 is
`same` with `notModified: true`. Crawl targets do not send conditional requests.

**Adaptive interval.** Each URL's changes are modelled as a Poisson process; the rate has a Gamma prior
(shape 1, mean one change per day) updated with every "changed / unchanged since Δ" observation. The
δ rule gives each URL the time at which the posterior probability of an unseen change reaches δ = 0.05;
their total request rate is the monitor's budget, split across its URLs as T_i ∝ λ_i^{-1/2} (the split
that minimises the mean detection delay for that budget), clamped to `minIntervalMs` (≥ 5 min) and
`maxIntervalMs`. Checks go through the scraper's per-host rate limits and robots.txt like any scrape.
Measured against fixed polling with the same number of requests: [benchmarks/history/0.95](../benchmarks/history/0.95/README.md).

Limits: 1000 monitors per account, 10,000 URLs per monitor, a crawl target's `limit` at most 10,000, and intervals of at
most 366 days. Checks of different monitors run independently, so one slow crawl or unreachable webhook does not delay the
others. A monitor whose check crashes moves to status `paused` with the warning `monitor_paused`.

Monitors are kept in the job store when one is configured and resume after a restart. With several
servers only the gateway that holds the lease schedules checks. Monitors are not replicated to the
warm standby: after a takeover, monitors created on the old gateway are not on the new one until they
are created again.

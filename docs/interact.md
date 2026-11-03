# Interact after scraping (`/v2/scrape/{id}/interact`)

`POST /v2/scrape/{id}/interact` continues on a browser page from an earlier `POST /v2/scrape`:
click, type, press keys, scroll, then read the page again. State carries over between calls.
`DELETE /v2/scrape/{id}/interact` closes the page. The shape follows
[Firecrawl v2](https://docs.firecrawl.dev/features/interact); the differences are listed below.
Both routes sit behind the same API keys, rate limits and quotas as the other Firecrawl routes.

## The scrape

Every `POST /v2/scrape` response carries `data.metadata.scrapeId` when the server has Chrome.

| Request | What happens |
|---|---|
| `keepSession: true` (pagesift extension) | The page is rendered in Chrome and stays open. Interact continues on that exact page. Cannot be combined with `engine: "http"`. Results are neither read from nor written to the cache |
| without it | Only the URL and the options are remembered. The first interact opens the URL again, in Chrome, and adds the warning `session_replayed`: the page may differ from the scrape result |

`keepSession` is only accepted on `POST /v2/scrape` and on the MCP tool `pagesift_scrape`; anywhere else it
is answered with `400 INVALID_REQUEST`. `/v1/scrape` responses have no `scrapeId`.

## The request

| Field | pagesift |
|---|---|
| `actions` | pagesift extension, instead of `code`: the action list documented for scrape (`wait`, `click`, `write`, `press`, `scroll`, `screenshot`, `scrape`, `executeJavascript`, `pdf`). `executeJavascript` follows the same server switch as on scrape (off by default) |
| `prompt` | 1–10,000 characters; needs `[llm]`, otherwise `400 UNSUPPORTED_OPTION`. Send `prompt` or `actions`, not both |
| `timeout` | seconds, 1–300, default 30 |
| `url` | when the id has no record any more (expired, or the server restarted): opens this URL in a new session under the same id |
| `code`, `language` | `400 UNSUPPORTED_OPTION`: there is no Playwright sandbox |
| `origin` | accepted, no effect |
| anything else | ignored, and listed in the warning `ignored_fields` (for example `zeroDataRetention`) |

`write` needs a focused element: click it first. Its `text` may contain `{{secret:NAME}}` (below).

### `prompt`

One call does one round: the model reads a snapshot of the page, chooses steps, pagesift performs them, and when
steps ran the model reads the new snapshot and answers. So a call makes one LLM request when no step is needed and
two otherwise. There is no retry loop and no cache of plans; split longer tasks into several calls.

- The snapshot comes from Chrome's accessibility tree (`Accessibility.getFullAXTree`), main frame only: one line per
  element with an id, role, name and value. Its token budget is derived from the model's context length and token
  rate limit, as for the `extract` format.
- The model can click, write, press, scroll and wait. It cannot run scripts.
- A step the page cannot carry out (the element is gone, the id is not in the snapshot) ends the call with
  `success: false`; steps already done stay done.

### Secrets

`{{secret:NAME}}` in `actions[].text` or in text the model types is replaced locally with the value of `NAME` from
`[secrets]` in `pagesift.toml`.

- A model may use only names that appear in the `prompt`, so text on a page cannot make it use another secret.
- Values do not reach the model: once used in a session, a value is replaced by its placeholder in later snapshots.
- Values do not reach the response: the same replacement runs over the page, `actions` output, errors and URLs.
  Screenshots and PDFs are images of the page and are not redacted.

## The response

| Field | pagesift |
|---|---|
| `success` | `false` when this call did not finish (selector not found, timeout). The session stays open. HTTP status is 200 |
| `exitCode` | `0`, or `1` when `success` is false |
| `killed` | `true` when `timeout` was reached |
| `error` | why `success` is false |
| `output` | the model's answer to `prompt` |
| `result` | `prompt`: the last page snapshot; `actions`: the value of the last `executeJavascript` |
| `stdout`, `stderr` | always empty |
| `cdpUrl`, `liveViewUrl`, `interactiveLiveViewUrl` | always `null`, with the warning `live_view_unavailable`. There is no live view, and the browser's debugging endpoint is not exposed |
| `data` | pagesift extension: the page after the call, in the non-LLM formats of the scrape (`markdown`, `html`, `rawHtml`, `links`, `images`), Markdown if there are none |
| `actions` | pagesift extension: `screenshots`, `scrapes`, `javascriptReturns`, `pdfs` |
| `steps` | pagesift extension: the steps `prompt` performed (action, element role and name; never the typed text) |
| `llmUsage` | pagesift extension: `{calls, tokens}` |
| `warnings` | pagesift extension |

`DELETE` answers `{success, sessionDurationMs, creditsBilled}`; `creditsBilled` is always 0.

## Errors

| Status | `code` | When |
|---|---|---|
| 400 | `INVALID_REQUEST`, `UNSUPPORTED_OPTION` | bad request; no LLM for `prompt`; `code`; no Chrome; a cluster gateway (below) |
| 403 | `FORBIDDEN` | the scrape belongs to another API key's account |
| 403 | `ROBOTS_DISALLOWED` | robots.txt disallows the page; the page's content is not returned and the session ends |
| 404 | `NOT_FOUND` | no such id, or its record expired. `DELETE` also answers 404 when no session is open |
| 410 | `SESSION_GONE` | the session was stopped or reclaimed, the browser exited, or the id comes from an earlier server process (the server restarted). The message says which |
| 429 | `CONCURRENCY_LIMIT_EXCEEDED` | no room for another session and none is idle |
| 502 | `UPSTREAM_ERROR` | the LLM or the browser failed |

Calls on one session run one at a time; a call waiting for its turn counts against its own `timeout`.

## How long a session lives

| Limit | Value |
|---|---|
| Idle, when memory is plentiful | 5 minutes (Firecrawl's documented default) |
| Total, from opening | 10 minutes (Firecrawl's documented default), however active the session is |
| Sessions the machine admits | `(available memory − reserve) / memory per tab + open sessions`, at most the browser pool's tab limit minus one |
| Idle, as the machine fills up | `5 min × (1 − (open − 1) / admitted)`; 0 when more are open than admitted |

Available memory, the reserve and the memory per tab are the numbers the browser pool's admission already measures
(`MemAvailable`, the PSS of its own browsers, how fast other processes grow). With
`min_available_memory_mb = 0` only the tab limit applies. Sessions are checked every 15 seconds (5 minutes × δ, δ = 0.05),
least recently used first, until the rest fit.

A kept page holds one tab of the pool but does not count as load: it does not hold back other renders, and a browser
full of kept pages does not count towards the pool's browser target.

Sessions live in the memory of one server process. Records of scrapes without an open page are limited to the memory of
one browser with one tab, oldest first.

## What stays in force on a kept page

- Network checks: the page's request interception keeps running, so navigations and subrequests started by clicks pass
  the same private-network rules as the first load.
- robots.txt: before each call for the current page, and again when a call navigated elsewhere.
- Per-host rate limits: each call that acts on the page takes one permit for the page's host. Subrequests the page sends
  by itself are not counted, as with rendering.

## Engines

Only Chrome (or Chromium, or a remote endpoint treated as Chrome) keeps sessions. Other engines in the chain are skipped for
`keepSession` and for replays. The accessibility tree, mouse and keyboard paths are tested on Chrome only.

## Several servers

Sessions are not available through a cluster gateway. Pages there run on whichever worker is free and the next request
may land elsewhere, so `keepSession`, `POST` and `DELETE .../interact` answer `400 UNSUPPORTED_OPTION`, and `/v2/scrape`
responses from a gateway have no `scrapeId`. Use a standalone `pagesift serve` for sessions.

## MCP, CLI, bindings

| Entry | pagesift |
|---|---|
| MCP | `pagesift_scrape` takes `keepSession`; `pagesift_interact` (`scrapeId`, `prompt` or `actions`, `timeout`) and `pagesift_interact_stop` (`scrapeId`). They appear only when the engine supports sessions. `pagesift_scrape` without `keepSession` returns no `scrapeId` |
| CLI | none: a session lives in the server process and would end when the command exits |
| Python, Node | not exposed |

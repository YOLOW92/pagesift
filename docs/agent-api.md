# Research agent (`/v2/agent`)

`POST /v2/agent` answers a question from the web when you do not know the URLs. It follows the shape of
[Firecrawl's `/agent`](https://docs.firecrawl.dev/features/agent), runs on the model you configure, and
sits behind the same API keys, rate limits and quotas as the other routes. It needs `[llm]` in
`pagesift.toml`, and `[search]` unless the request gives `urls`; without them the request is answered with
`400 UNSUPPORTED_OPTION`.

Each run repeats: the model plans one step → pagesift runs the tool (`search`, `map`, `scrape` with a query
filter, `extract`) → the text is archived as evidence → the run decides whether to go on. robots.txt, per-host
rate limits and the SSRF policy apply to every page; the model chooses only URLs and queries, not fetch options.

## Request

| Field | pagesift |
|---|---|
| `prompt` | Required, at most 10,000 characters |
| `urls` | Pages to start from |
| `schema` | JSON Schema of the answer. An answer that does not match gets one correction, then the run fails |
| `effort` | `low`, `medium` (default), `high`. Changes only the budget ceilings, not when the run stops |
| `strictConstrainToURLs` | Open only the pages in `urls`; `search` and `map` are not offered to the model |
| `maxCredits` | Stop after this many credits. One credit is one page read, one search or one map; model tokens are not credits |
| `webhook` | URL or object. Events `agent.started`, `agent.action`, `agent.completed`, `agent.failed`, `agent.cancelled` |
| `allowedHosts` | pagesift extension: open only pages on these hosts and their subdomains |

Other fields, including `model`, `threadId`, `mode`, `location`, `safe`, `exchange` and `zeroDataRetention`, are
accepted and listed in `warnings` as `ignored_fields: ...`: one model is configured per server, runs have no
threads, and results are stored for 24 hours like any other job.

| `effort` | Pages | Model calls |
|---|---|---|
| `low` | 5 | 12 |
| `medium` | 20 | 42 |
| `high` | 80 | 162 |

The token ceiling of a run is model calls × tokens per call. Tokens per call come from the service: the smaller
of the model's context length (`GET /models/{model}`) and the per-minute limit (`x-ratelimit-limit-tokens`),
times 0.95, split evenly between prompt and output, with the output capped at the model's own limit. Above the
per-minute limit the run waits. Without a reported context length, 8192 is assumed.

## Response

`POST` returns `{"success": true, "id": "..."}`, with `warnings` when fields were ignored. `GET /v2/agent/{id}` returns:

| Field | Meaning |
|---|---|
| `status` | `processing`, `completed`, `failed` |
| `success` | `false` on a failed run |
| `data` | The answer. Only on `completed` |
| `expiresAt`, `creditsUsed`, `effort`, `error` | As in Firecrawl |
| `stopReason` | `credit_limit_reached` when the budget ended before a complete answer (`failed`) |
| `partial`, `partialSchemaValid`, `message` | The incomplete answer on a failed run, and whether it matches `schema` |
| `finishReason` | pagesift extension: `answered`, `coverage_reached`, `saturated`, `no_relevant_pages`, `budget_exhausted`, `cancelled`, `error` |
| `complete`, `missing` | pagesift extension: there is an answer, it matches `schema`, and the model reported nothing as missing |
| `sources` | pagesift extension: `url`, `title`, `evidenceIds`, `contentHash`, `fetchedAt` |
| `fieldSources` | pagesift extension: for each leaf of `data`, by JSON Pointer: `sources`, `evidenceIds`, `verified` |
| `steps` | pagesift extension: each tool call with its input and result (`ok`, `REFUSED`, or an error code such as `ROBOTS_DISALLOWED`) |
| `usage` | pagesift extension: `pages`, `pagesFailed`, `searches`, `maps`, `llmCalls`, `tokens`, `tokensEstimated` |
| `budget` | pagesift extension: `maxPages`, `maxLlmCalls`, `maxTokens`, `maxCredits`, `inputTokensPerCall`, `exhausted` |
| `evidence` | pagesift extension, with `?includeEvidence=true`: the archived text the model saw (`id`, `url`, `kind`, `query`, `text`, `relevance`) |
| `warnings` | See below |

`DELETE /v2/agent/{id}` cancels between two steps; a step already running finishes first. A cancelled run reports
`failed`. Finished runs survive a restart when the server has a store. A run that was in progress at a restart is
not resumed and reports `failed`. Jobs are visible only to the API key that started them.

## When a run stops

- The model finishes on its own (`answered`).
- The adaptive crawl's stopping rule fires: new pages stop adding relevant information (`saturated`,
  `coverage_reached`), or only pages without any sign of the query were reached (`no_relevant_pages`). The rule
  and its defaults are the same as `adaptive` on `/v2/crawl`. The model then has to answer from the evidence.
- The budget ends: pages, credits, the caller's page quota, model calls or tokens. The model has to answer from
  the evidence. If the answer lists something as missing, does not match `schema`, or has a field whose value is
  not in the evidence, the run reports `failed` with `stopReason: credit_limit_reached`, `budget.exhausted` and
  `partial`. If there are no tokens left to answer at all, only the evidence remains.

A run stopped by the adaptive rule with an incomplete answer is `completed`, with `complete: false`, `missing` and
the warning `agent_incomplete`.

## Sources

The model cites evidence ids for each field. pagesift then checks every value against the evidence text, word for
word, ignoring case and thousands separators. A wrong citation is replaced by the evidence that does contain the
value. A value found nowhere keeps the model's citation, gets `verified: false` and is listed in
`agent_unverified_fields`. A summary in the model's own words and a boolean are never `verified`.

The text kept as evidence is the part of the page that matches the model's query (the query filter, on the whole
page rather than only the main content, up to a quarter of the prompt budget per page), a search result's
description, or the items of an `extract`. Later turns do not repeat it; the model carries facts forward in its
own notes, and only the last turn sees all the evidence that fits.

## Warnings

| Code | Meaning |
|---|---|
| `ignored_fields` | The request had fields nobody reads |
| `agent_robots_disallowed` | robots.txt refused this URL; it was not fetched |
| `agent_unverified_fields` | These fields' values were not found in the evidence |
| `agent_incomplete` | What is missing from the answer |

## Differences from Firecrawl

- The model is yours (`[llm]`); `model` and `mode` are ignored. Search uses your `[search]` backend.
- Credits count pages, searches and maps, not reasoning. A failed run keeps its `creditsUsed`.
- No threads, trace and snapshot endpoints, run list, Alexandria providers, proxy countries or ZDR.
  `steps` and `evidence` are in the status response instead.

## Other entry points

- CLI: `pagesift agent "PROMPT" [--urls URL...] [--schema FILE] [--effort low|medium|high]
  [--strict-constrain-to-urls] [--max-credits N] [--allowed-hosts HOST...] [--include-evidence]`. Prints the same
  JSON as `GET /v2/agent/{id}`, writes each step to stderr, and exits 1 when `status` is `failed`.
- MCP: `pagesift_agent` and `pagesift_agent_status`, listed only when `[llm]` is configured. No webhooks.
- The multi-server gateway runs the same loop; pages go through its workers.
- The Python and Node bindings have no LLM or search configuration, so they have no agent call.

Page content goes to your model as data with an instruction not to follow it, but a model can still be misled by
a page. For sensitive runs, limit where it can go with `allowedHosts` or `strictConstrainToURLs`.

Test method and flow results: [`benchmarks/history/1.0/agent/`](../benchmarks/history/1.0/agent/README.md).

# 0.97 intent → schema → cache: live run

2026-11-03, `pagesift serve` (quick build), `[llm]` = Groq, model `openai/gpt-oss-20b`
(`openai/gpt-oss-120b` was the first choice; its 200 000 tokens-per-day quota on this account was used
up while the snapshot budget was being fixed, see below). Each site scraped twice with
`{"type": "extract", "intent": ...}`, `maxAge: 0`. Raw results: `live_groq.json`.

| Site | Intent | First run | LLM calls first / repeat | Tokens first (estimate) |
|---|---|---|---|---|
| quotes.toscrape.com | quote text and author | 10 items, all fields filled | 1 / 0 | 1034 |
| news.ycombinator.com | story titles and their links | 30 items, all fields filled | 1 / 0 | 1442 |
| books.toscrape.com | book titles and prices | 20 items, prices right, titles empty (the title is in an attribute) | 1 / 0 | 1216 |
| www.python.org | upcoming events with dates | failed after one correction (`baseSelector` matched nothing) | 2 / – | – |
| en.wikipedia.org (Rust) | release versions and dates from the infobox | failed | – | – |

Success (every field filled on the first run, ↑): 2 of 5; partial: 1 of 5. Repeat runs used the
cached schema with 0 LLM calls on every site that succeeded (↓ is better). Tokens are pagesift's
estimate (bytes / 3, calibrated on this tokenizer: 6033 bytes = 1969 tokens) of prompt + reply; the
model's own reasoning tokens are not included.

The snapshot budget is the smaller of the model's context window and the account's tokens-per-minute
limit (read from `x-ratelimit-limit-tokens`, 8000 here), times 1 − δ, minus the prompt, split evenly
between input and output.

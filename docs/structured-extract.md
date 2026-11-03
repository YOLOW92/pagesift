# Structured extraction (`extract` format)

`extract` pulls structured data out of a page with a CSS schema, without an LLM. It is a pagesift
extension format; Firecrawl's `json` format is a different thing (it asks the configured LLM).

| Entry point | Form |
|---|---|
| HTTP (`/v1`, `/v2` scrape, batch, crawl) | `"formats": [{"type": "extract", "schema": {...}}]`; result in `data.extract` |
| MCP `pagesift_scrape` | same `formats` object form |
| CLI | `pagesift extract --schema schema.json URL...` — one JSON line per URL; pages are always fetched fresh, selector memory uses the store |
| Python | `Scraper.extract(urls, schema)`, `Scraper.extract_iter(urls, schema)`, or `extract_schema=` on any scrape |
| Node | `scraper.extract(urls, schema)`, `scraper.extractStream(urls, schema)`, or `extractSchema` on any scrape |

The schema uses the Crawl4AI `JsonCssExtractionStrategy` field names:

```json
{
  "baseSelector": "li.product",
  "fields": [
    {"name": "name", "selector": "h3", "type": "text"},
    {"name": "url", "selector": "a", "type": "attribute", "attribute": "href"},
    {"name": "price", "selector": ".price", "type": "regex", "pattern": "([\\d.]+)"}
  ]
}
```

The result is an array with one object per element matched by `baseSelector`; items where no field
matched are dropped. An invalid schema (bad selector, bad regex, missing `baseSelector`) is rejected
with `INVALID_REQUEST` before the page is fetched.

## Selector memory and relocation

When a store is open, pagesift remembers, for each registered domain + schema + field, the element's
structural fingerprint and the selector that last matched. The schema id is a hash of the schema JSON,
so editing the schema starts a fresh memory.

| Entry point | Store |
|---|---|
| HTTP (`serve`), MCP (http and stdio), CLI `extract` | `[cache]` in `pagesift.toml`, on by default. One process can hold a data directory: when another one has it, a warning goes to stderr and that process runs without cache and memory |
| Python | `Scraper(store_path=...)`; no store by default |
| Node | `new Scraper({ storePath })`; no store by default |

Relocation covers `baseSelector` and top-level fields that have a `selector`. Sub-fields of `nested`,
`list` and `nested_list` fields, `source` fields and `baseFields` are extracted exactly, never
relocated, and a failed one produces no warning.

When a selector stops matching, `pagesift_extract::locate` looks for the element again, in this order,
and stops at the first step that decides:

1. **exact** — the selector (or the remembered current selector) matches exactly one element;
2. **anchor** — if the field names an identity attribute (`"identity": "data-sku"`), find the element
   carrying the value seen last time; none is `missing`, several are `ambiguous`; similarity is never
   used as a fallback. An element without that attribute is not remembered;
3. **fingerprint** — compare every element with the remembered fingerprint, feature by feature
   (tag, stable attributes, non-hashed class names, text, ancestors, sibling text, child tags; Dice
   coefficients). Each feature's similarity is turned into evidence by asking how many elements of the
   same page are at least as similar on it (an empirical tail probability q, likelihood ratio ≤ 1/q);
   a feature the redesign destroyed (all classes hashed) becomes neutral instead of counting against
   the element. The best element is accepted only when its posterior probability of being the target,
   against all other elements (its own ancestors and descendants excluded), is at least 1 − δ with
   δ = 0.05. List items with the same structure as the best one are not counted as competitors when
   relocating `baseSelector`; for fields, the evidence is estimated across all list items.

After a successful relocation a new selector is generated (stable `id`, test attributes such as
`data-testid`, semantic attributes, then non-hashed class names, and `:nth-of-type` only as a last
resort), checked to match exactly the relocated element (or list), written back to the memory, and
reported in `metadata.pagesift.warnings`:

| Warning | Meaning |
|---|---|
| `relocated: <field> <old> -> <new>` | the old selector no longer matched; the element was found again and `<new>` is used from now on |
| `extract_missing: <field>` | the selector does not match and the element could not be found |
| `extract_ambiguous: <field>` | several candidates fit equally well; pagesift does not guess |

Without a store the extraction is exact only (no relocation); a selector that matches nothing still
reports `extract_missing`. When `[llm]` is configured, whatever is still unresolved (`baseSelector`
first, then fields) goes to the model once, with or without a store, and a selector that then matches
reports `selector_healed: <field> via llm`. Each unresolved field costs a model call on every run,
including a field that is legitimately absent from the page. Fields that only have `describe` are
resolved by the model on first use; with a store the result is cached per registered domain + URL
shape + schema, so repeat runs make no model call.

Unknown keys in a schema or a field are rejected with `INVALID_REQUEST`.

## Finding similar elements

From one example element, find its structural siblings (same tag, semantic classes and child tags),
so a single example is enough to address a whole list. The walk goes up from the example until a level
has look-alike siblings; `depth` says how many levels (0 = the example itself).

| Entry point | Form |
|---|---|
| HTTP | `POST /v2/similar` with `{"url": ..., "selector": <CSS of the example>, "engine": "auto"}` |
| CLI | `pagesift similar URL --selector CSS` — one JSON object |

The result is `{"selector", "depth", "items": [{"text", "html"}]}`. `selector` is null when none could
be built that picks out exactly those items; `depth` is null and `items` empty when the example has no
structural siblings. A selector that matches nothing is `INVALID_REQUEST`.

## Observe

`POST /v2/observe` with `{"url": ..., "instruction": ...}` needs `[llm]` and returns candidate elements,
best first: `{"success": true, "data": [{"selector", "role", "name"}], "warnings": ["llm_usage: ..."]}`.

## Measured accuracy

On 916 holdout cases (synthetic redesigns of 27 live pages and 24 real 2017 → 2024 redesigns from
Common Crawl), `locate` returned the right element in 53.3% of the cases and a wrong one in 18.7%,
against 46.5% and 53.5% for Scrapling's `adaptive` mode on the same cases; median 2.9 ms per field
against 36.1 ms. Method, per-kind numbers and scripts: [benchmarks/history/0.93](../benchmarks/history/0.93/README.md).

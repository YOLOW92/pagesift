/*
 * pagesift.h: C ABI of libpagesift. JSON in, JSON out.
 *
 * Strings are NUL-terminated UTF-8. Every function that returns `char *` hands back a JSON
 * string the caller must release with pagesift_string_free() (not free()).
 *
 * Errors are data, not return codes:
 *   - every JSON result has a boolean "success";
 *   - on failure it also has "error_code" (e.g. "INVALID_REQUEST", "UNSUPPORTED_OPTION",
 *     "ROBOTS_DISALLOWED"), "error_message" and "error": {code, message, details, retry_after};
 *   - unknown option keys are an error (INVALID_REQUEST), never silently ignored;
 *   - a Rust panic never crosses this boundary, it comes back as INTERNAL_ERROR.
 * NULL comes back only from pagesift_engine_new() on failure, or if a result cannot be allocated.
 *
 * Thread safety:
 *   - A pagesift_engine may be shared and called from any number of threads at once. Create one
 *     and reuse it: it owns the connection pool, per-host rate limits and the browser pool.
 *   - Calls block the calling thread until done; concurrency runs on an internal tokio runtime
 *     shared by all engines (started on first use). Do not fork() after the first call.
 *   - pagesift_engine_free() must not race with any other call on the same engine.
 *   - pagesift_convert() and pagesift_version() need no engine and are always thread-safe.
 *   - Arguments are only read during the call; nothing is retained.
 *
 * Option keys are the same as the Python bindings (snake_case). options_json may be NULL.
 */
#ifndef PAGESIFT_H
#define PAGESIFT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct PagesiftEngine pagesift_engine;

/* Version, e.g. "1.0.0". Static string: do not free. */
const char *pagesift_version(void);

/* Releases a string returned by this library. NULL is a no-op. */
void pagesift_string_free(char *s);

/*
 * Creates an engine. config_json may be NULL. Keys, all optional: user_agent,
 * allow_private_network (default false; needed for localhost), allow_cidrs, proxy,
 * request_interval_ms (1000), per_host_concurrency (2), max_concurrency (64), browser_executable,
 * browser_url, max_browsers (2), store_path, ocr (false), ocr_quality, ocr_formulas, models_dir, gpu.
 *
 * On failure returns NULL and, when error_out is not NULL, stores a JSON error string there
 * (release it with pagesift_string_free). *error_out is NULL on success.
 */
pagesift_engine *pagesift_engine_new(const char *config_json, char **error_out);

/* Shuts down the browser pool and frees the engine. NULL is a no-op. */
void pagesift_engine_free(pagesift_engine *engine);

/*
 * Scrape options, accepted by pagesift_scrape, pagesift_parse, pagesift_batch, pagesift_crawl:
 * formats (array of markdown, html, raw_html, links, images), only_main_content (true),
 * engine (http, browser, auto), timeout (ms), wait_for (ms), headers, include_tags, exclude_tags,
 * ignore_robots_txt, mobile, languages, proxy, if_none_match, if_modified_since, export_images,
 * extract_schema (object), max_age (ms), parsers (array).
 *
 * Document JSON: index, url, final_url, success, status_code, markdown, html, raw_html, links,
 * images, extract, metadata, attachments ([{id, format, bytes, hash, source, data}], data is base64),
 * error_code, error_message, error, engine, content_hash, raw_hash, text_hash, elapsed_ms, timings,
 * warnings, ocr, redirect_chain, etag, last_modified, not_modified.
 */

/* Scrapes one page. Returns a Document. */
char *pagesift_scrape(const pagesift_engine *engine, const char *url, const char *options_json);

/*
 * Parses a file held in memory (PDF, HTML, Office, EPUB, CSV, text...). Returns a Document.
 * The type is detected from the content, then filename, then content_type. data may be NULL when
 * len is 0. Extra option keys: content_type, url (base for relative links; never fetched).
 */
char *pagesift_parse(const pagesift_engine *engine, const uint8_t *data, size_t len,
                     const char *filename, const char *options_json);

/* Converts HTML to Markdown. Returns a Document. Option keys: url, only_main_content (true). */
char *pagesift_convert(const char *html, const char *options_json);

/*
 * Scrapes many pages. urls_json is a JSON array of strings. Extra option key: concurrency (16).
 * Returns {"success": true, "documents": [Document...]} in input order. A failed page does not
 * fail the batch: its Document has "success": false.
 */
char *pagesift_batch(const pagesift_engine *engine, const char *urls_json,
                     const char *options_json);

/*
 * Lists a site's links. Option keys: limit, search, sitemap (include, skip, only),
 * ignore_robots_txt. Returns {"success": true, "links": [{url, title, description}...]}.
 */
char *pagesift_map(const pagesift_engine *engine, const char *url, const char *options_json);

/*
 * Crawls a site and returns when the crawl has finished; all pages are held in memory until then.
 * Extra option keys: limit, max_depth, include_paths, exclude_paths, crawl_entire_domain,
 * allow_subdomains, sitemap, query, adaptive, max_concurrency.
 * Returns {"success": true, "documents": [Document...], "summary": {stop_reason, pages, coverage,
 * term_evidence}}. Documents are in completion order. If the crawl itself fails, the result has
 * "success": false plus the "documents" gathered so far.
 */
char *pagesift_crawl(const pagesift_engine *engine, const char *url, const char *options_json);

#ifdef __cplusplus
}
#endif

#endif /* PAGESIFT_H */

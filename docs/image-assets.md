# Image assets

Image assets are opt-in. Without `imageAssets`, image links and the existing `exportImages` behavior are unchanged. Set `imageAssets` on a scrape or batch request, or in `scrapeOptions` on a crawl request:

```json
{"url":"https://example.com/", "imageAssets":"rewrite"}
```

- `attach`: Markdown keeps its links; `attachments` carries Base64 image bytes.
- `embed`: supported images in Markdown become `data:` URLs. Each page is independently usable offline, so the same image can occur on multiple pages.
- `rewrite`: Markdown uses `asset://<id>` and `attachments` carries the bytes.

Each `imageAssets` result has a `source` and a `status`: `ok`, `skipped-limit`, or `failed`. Successful results include `id`, `mime`, and `bytes`; unsuccessful results include a `reason`. Attachment IDs use the first 32 hexadecimal characters of the SHA-256 of the exported bytes, followed by a format extension; `hash` contains the full SHA-256. MIME types are identified from image bytes, not just filenames or HTTP headers.

In batch and crawl results, attach/rewrite attachments with the same hash carry Base64 `data` only on the first published page; later attachment metadata and `imageAssets` refer to the same ID. Consumers of paginated results must retain the first asset or use the attachment endpoint while it is available. The endpoint is scoped to the caller that received the attachment; another key, an unknown ID and an expired attachment all return the same `404`. For the CLI, `scrape` and `crawl` accept `--image-assets attach|embed|rewrite`. With `--out`, image files are stored under `images/<id>` and per-image outcomes appear in `index.jsonl`.

Web images are fetched using the configured network and robots policy, with at most four image requests in flight per page. Retrieval has a shared per-page byte budget, at most 64 image requests, and a 10-second overall image-fetch deadline; skipped and failed images retain their original Markdown links. The ordinary JSON response is returned after image retrieval.

For early text, send the same JSON request to `POST /v2/scrape/stream`. The response is an SSE stream. With `imageAssets` enabled, `event: markdown` carries a JSON object containing provisional `markdown` and a `truncated` flag immediately after extraction and before image retrieval. `event: done` carries the complete scrape response, including the final Markdown and image assets; `event: error` carries an error if the scrape fails. The early Markdown is not the final transformed result: image links have not yet been rewritten and options such as query filtering can change the final Markdown. Previews are capped at 1 MiB, with `truncated: true` when shortened. Without `imageAssets`, the stream emits the final result only. After the SSE response starts, errors are sent as events rather than HTTP error statuses.

Batch and crawl clients can opt in to provisional events by adding `?previews=true` to their existing `/stream` URL. These `markdown` events include the page `url`, `markdown`, and `truncated`. They have no sequence ID, are not persisted or replayed, and can be dropped for a slow or disconnected reader. Final `page` events retain their existing sequence IDs and resume behavior.

During a batch or crawl, up to 32 image URLs can share downloaded images of at most 512 KiB each in a task-local cache. On a persistent task restart, the cache is repopulated from up to 32 saved successful image sources. Larger images, additional URLs, and images not present in saved results can be requested again. Different URLs must be fetched before their content hashes can be compared.

PDF image assets cover exported XObject images, independently of OCR. PDF inline images are not included. Office ZIP media are exported using their original bytes when their format is recognized. Email image attachments are not included in this asset pipeline.

SVG is supported only as an attachment or asset reference; the new embed mode does not insert SVG data URLs into Markdown. SVG identification is not sanitization: consumers must treat SVG bytes as potentially active content. Existing source-page data URLs are controlled separately by `removeBase64Images`.

MCP scrape returns recognized assets as native image content. With `embed`, the text block keeps its self-contained Markdown; when that Markdown (or a PDF page) contains image data URLs, the corresponding Markdown field is omitted from `structuredContent` to avoid another copy. The text and native image blocks can still contain the same image bytes because they serve different MCP consumers.

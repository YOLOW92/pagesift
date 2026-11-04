# File parsing: `POST /v2/parse` (and documents fetched by scrape)

Upload a file's bytes directly, without first putting it at a reachable URL, and get the same document as
`POST /v2/scrape`. The request format follows Firecrawl's current `/v2/parse`
([API reference](https://docs.firecrawl.dev/api-reference/endpoint/parse),
source `apps/api/src/controllers/v2/parse.ts`). MCP does not offer this endpoint: files do not travel over MCP.

Supported files:

| Format | Extensions | Conversion |
| :--- | :--- | :--- |
| PDF | `.pdf` | pagesift-pdf: text layer, and OCR for scanned pages when turned on |
| HTML | `.html`, `.htm`, `.xhtml` | The same main-content extraction as web pages |
| Word | `.docx` (also docm, dotx) | pagesift-office: headings, lists, tables, image alt text, links, footnotes, formulas (LaTeX) |
| Excel | `.xlsx` (also xlsm, xltx) | One second-level heading and one table per sheet; cells are values as displayed by their number format |
| PowerPoint | `.pptx` (also pptm, ppsx) | `<!-- Slide N -->` per slide, titles, bullets, tables, images, chart data, speaker notes |
| EPUB | `.epub` | In spine order; each chapter goes through the HTML extractor, keeping the whole chapter |
| CSV | `.csv`, `.tsv` | One table (RFC 4180, delimiter detected). Charset as for web pages: BOM, `charset` in Content-Type, then detection from the body; a declaration that disagrees with the body is corrected with a `decode:` warning |

| Legacy Office | `.doc`, `.xls`, `.ppt`, `.xlsb` | Word: headings, lists, tables, footnotes. Excel: same output as `.xlsx`. PowerPoint: slide markers, text, notes. Word 6/95 is text only |
| OpenDocument | `.odt`, `.ods`, `.odp`, flat `.fodt`, `.fods`, `.fodp`, `.fodg` | Headings, lists, tables, one heading per sheet, slide markers |
| Other word processors | `.rtf`, `.wpd`, `.wp`, `.hwp`, `.hwpx` | Text and tables where the format has them. WordPerfect for Mac 1.x and 3.x is not read |
| E-books | `.mobi`, `.azw`, `.azw3`, `.prc`, `.fb2` | MOBI goes through the HTML extractor; DRM reports that the file is encrypted |
| Email | `.eml`, `.mbox`, `.msg` | Headers, body, attachments converted when their type is supported |
| Archives | `.zip`, `.tar`, `.tgz`, `.gz`, `.bz2`, `.xz`, `.7z` | A file list, then each supported member. Nested up to 3 levels, 1,000 members |
| Images | `.png`, `.jpg`, `.gif`, `.bmp`, `.tif`, `.webp` | OCR when it is on; otherwise `UNSUPPORTED_OPTION`. Animated WebP returns a clear error |
| Databases | `.sqlite`, `.db`, `.gpkg`, `.dbf` | One table per database table, read from the file format, no SQL is run. Blobs show their size; dBASE memo files are not read |
| XML documents | `.dbk`, `.docbook`, `.jats`, `.nxml`, `.svg`, `.opml` | Sections, lists and tables; SVG keeps text only |
| Text and notebooks | `.md`, `.txt`, `.rst`, `.adoc`, `.org`, `.tex`, `.typ`, `.bib`, `.ris`, `.srt`, `.vtt`, `.ipynb` | As written; subtitles lose their timings; notebooks keep cells and text outputs |
| Data | `.json`, `.jsonl`, `.yaml`, `.toml`, `.xml`, `.ini` | One fenced code block |
| Source code | 373 languages by extension or file name | One fenced code block with the language tag. C, C++, C#, Go, Java, JavaScript, TypeScript, Lua, PHP, Python and Rust are split into one section per top-level function or class, with function, class and import names in the metadata. Other languages, and files that are over 2 MB or do not parse cleanly, stay one block |

`/v2/scrape` uses the same conversion code when it fetches these formats (see the end of this page), except text, data
and source code, which stay web pages when fetched. `pagesift scrape <local file>` uses it too.

## Request

`multipart/form-data` with these fields:

| Field | Required | Content |
| :--- | :--- | :--- |
| `file` | Yes | The file, up to **50 MB**. Its name becomes `metadata.sourceURL` (last path segment only). |
| `options` | No | A JSON object as a string, with the scrape options below. |
| `url` | No | pagesift extension: the http(s) address the uploaded HTML came from. Relative links resolve against it (a `<base href>` in the page still wins) and it becomes `metadata.sourceURL`. It is only used to resolve addresses and is never fetched. Without it, relative links resolve to `file:///…` and stay out of `links`. |

```bash
curl -s http://localhost:3002/v2/parse \
  -H 'Authorization: Bearer sk-test' \
  -F file=@report.pdf \
  -F 'options={"formats":["markdown"],"parsers":[{"type":"pdf","maxPages":50,"pages":true}]};type=application/json'
```

How the file type is decided: content starting with `%PDF-` is a PDF. A ZIP container is recognized by its parts: Word
(`word/document.xml`), Excel (`xl/workbook.xml`), PowerPoint (`ppt/presentation.xml`), EPUB (`META-INF/container.xml`), and
a legacy Office or HWP file by the streams inside its OLE container, so a wrong file name or type does not matter. A
password-protected OOXML file (also an OLE container) is recognized by its extension and reports that it is encrypted. Anything else goes by extension, then by this field's Content-Type. CSV has
no signature, so it needs the extension or `text/csv`.

### options

| Field | Default | Notes |
| :--- | :--- | :--- |
| `formats` | `["markdown"]` | `markdown`, `html`, `rawHtml`, `links`, `images`; with a configured model also `summary`, `json`. `screenshot`, `branding`, `changeTracking` are not supported. PDF and Office documents have only `markdown` (see also `parsers: []`). |
| `onlyMainContent` | `true` | For HTML: keep the main content only. |
| `includeTags` / `excludeTags` | none | As in scrape. |
| `timeout` | `30000` | Milliseconds, 1000–300000. A timeout returns 408 `TIMEOUT`. |
| `parsers` | `["pdf", "image"]` | See below. |
| `queryFilter`, `chunking` | none | pagesift extensions, as in scrape. |
| `proxy` | none | Only `basic` and `auto` are accepted (no network is involved, so they do nothing). |
| `headers`, `skipTlsVerification`, `removeBase64Images`, `blockAds`, `origin`, `integration` | | Accepted, no effect on the result. |
| Other fields (such as `zeroDataRetention`) | | Accepted, no effect, listed in `metadata.pagesift.warnings` as `ignored_fields: <name>`. |

Options that cannot apply return 400 `UNSUPPORTED_OPTION`: `actions`, `waitFor`, `location`, `mobile`,
`engine: "browser"`, `egress`, `discoverScripts`, conditional requests, and `proxy` other than `basic` or `auto`.

### `parsers` (the same for parse and scrape)

As in Firecrawl, each item is a parser name (`"pdf"`, `"image"`) or an object (`{"type": "pdf", ...}`). Omitting it means
the default; with a list, parsers not in the list are off.

| Value | Effect |
| :--- | :--- |
| Omitted, `["pdf"]`, `["pdf", "image"]` | PDFs are parsed normally. `"image"` is accepted, but pagesift does not process images: image URLs still return `UNSUPPORTED_OPTION`. |
| `[]`, `["image"]` | PDFs are not parsed: as in Firecrawl, the original file goes into `markdown` as base64 (and into `html` if requested). |
| `{"type": "pdf", "mode": "fast"}` | Text layer only. A scan with no text layer returns 400 `UNSUPPORTED_OPTION` (Firecrawl: `SCRAPE_PDF_OCR_REQUIRED`). |
| `{"type": "pdf", "mode": "auto"}` | Default. Reads the text layer first. With OCR turned on (`[ocr] enabled`), scanned pages go through OCR; otherwise the result returns as usual with a `scanned_pages_without_ocr` warning. |
| `{"type": "pdf", "mode": "ocr"}` | Every page with an image goes through OCR, and recognized text replaces that page's text layer; pages with no image keep theirs. Without OCR turned on this returns `UNSUPPORTED_OPTION`: a request can use OCR but not turn it on, since turning it on downloads models. |
| `{"type": "pdf", "maxPages": N}` | 1–10000. **Parses only the first N pages**, with a `max_pages` warning. `metadata.numPages` is the pages converted, `metadata.totalPages` the pages in the file (as in Firecrawl). |
| `{"type": "pdf", "pages": true}` | Adds Markdown per page: `data.pages: [{pageNumber, markdown}]` (scrape too). |
| `blocks`, `pageMarkers` | Not supported; `true` returns `UNSUPPORTED_OPTION`. |

Invalid `parsers` (not an array, unknown items, values out of range) also return 400 in scrape, before anything is
fetched. `parsers` is part of the cache key (without it the key is unchanged).

### OCR

Turn it on with `[ocr] enabled = true` or `--ocr`. Models download when the first scanned image arrives, or ahead of time
with `pagesift models fetch`. `metadata.pagesift.ocr` reports `engine`, `model`, `device`, `pages` and `fallback`. Text
models use an NVIDIA GPU when one is available and fall back to the CPU with a `gpu_fallback` warning; layout and
formula models run on the CPU. `[ocr] formulas = true` also recognizes display formulas as `$$…$$`.

## Response

The same as `/v2/scrape`: `{"success": true, "data": {markdown, ..., metadata}}`.

- PDF: `markdown` is identical to fetching the same file with `/v2/scrape`, as are `metadata.title`, `numPages` and
  `totalPages`. When `/Info` has no `/Title`, `metadata.title` is the title inferred from the layout (the first page's
  top-level heading) and `metadata.titleSource` is `"inferred"`. A damaged file gives whatever can be recovered and says so:
  `pdf_repaired` (the cross-reference table was unusable or wrong, and objects were found by scanning the file) and
  `pages_missing` (page objects referenced by the page tree are gone, such as in a truncated file:
  `2 of 5 pages could not be recovered ...; 3 converted`, with `numPages` and `totalPages` counting recovered pages). The
  audit log counts `pages_missing` as a suspect result.
- HTML: extraction as in scrape. Without `<base href>`, relative links resolve against `file:///<file name>` and do not
  pretend to be any URL; `links` takes only http(s) links, so they stay out of it. An uploaded page that looks like a
  block page is still extracted, with an `access_barrier` warning (when fetching, this is an error).
- Office, EPUB, CSV: `metadata.title` comes from the document properties (docProps/core.xml, the OPF's dc:title).
  Notices such as truncation go into `metadata.pagesift.warnings`, and timing into `metadata.pagesift.timings.<extension>`.
- `metadata.sourceURL` is the file name, `statusCode` is 200, `contentType` is the format's MIME type (for HTML, the uploaded
  type), and `metadata.pagesift.engine` is `upload`.

## Errors

The body is `{"success": false, "code", "error"}` as on other endpoints. **PDF and Office documents fetched by scrape use the
same status codes.**

| Case | Status | `code` |
| :--- | :--- | :--- |
| Not multipart, malformed multipart, missing `file`, empty file, `options` is not a JSON object, invalid option values | 400 | `INVALID_REQUEST` (Firecrawl: `BAD_REQUEST`) |
| Unsupported file type, encrypted PDF/Office/EPUB (DRM), options that cannot apply | 400 | `UNSUPPORTED_OPTION` (Firecrawl, for types: `UNSUPPORTED_FILE_TYPE`) |
| Damaged file that cannot be read | 400 | `INVALID_REQUEST`, with `error` starting `could not read PDF` (or `DOCX`, `XLSX`, …) |
| File over 50 MB (a declared Content-Length over the limit is refused without reading the body); PDF over 5000 pages; decompressed size over the limit (below) | 413 | `TOO_LARGE` |
| Body not received in time (120 s by default, configurable); parsing longer than `timeout` | 408 | `TIMEOUT` |
| Wrong API key; over rate, concurrency or quota limits | 401 / 429 | As on other endpoints |

## Resources and safety

- Memory: one copy of the file in memory. The buffer is allocated once from Content-Length, filled as bytes arrive, and
  stopped at the limit. It is then handed to the parser as is and freed afterwards.
- Office and EPUB limits (`pagesift_office::Limits`): 20,000 ZIP entries; 256 MB decompressed per entry and 512 MB per file;
  at most 400:1 compression for entries over 4 MB (against ZIP bombs); 512 levels of XML nesting; 2,000,000 cells and 1024
  columns for tables (Excel, CSV), with the excess cut and a warning. XML does not expand entities declared in a DOCTYPE,
  so there is no external-entity (XXE) or entity-expansion problem.
- CPU: shares the parsing gate and per-core CPU slots with fetching, so many uploads cannot overwhelm the machine.
- Time limits: all four server limits can be set under `[server.timeouts]` in `pagesift.toml` (seconds, above 0):
  `header_read_secs` (reading headers, default 30), `body_secs` (reading the body, default 30), `upload_body_secs` (the
  `/v2/parse` body, default 120), `request_secs` (the whole request, default 360). The extra upload time is added to the
  request limit, so a slow upload does not eat into the parse `timeout`.
- Audit log: each parse writes `kind: "parse"`, `url` as `upload:<file name>`, and `fileBytes`. No snapshot is stored, since
  uploaded files are often not public.
- Metrics: `pagesift_pages_scraped_total{source="parse"}`, plus request counts and latency for the route `/v2/parse`.

## Documents fetched by `/v2/scrape`

Fetched content is classified in this order: PDF (starts with `%PDF-` or `application/pdf`) → Office/EPUB (parts inside the
ZIP, Content-Type, or the URL's extension) → CSV (`text/csv`, or a URL ending in `.csv`/`.tsv` when the server did not say
HTML) → everything else as a web page. Conversion is identical to uploads, with `metadata.pagesift.engine` as `http`.

## Deliberate differences from Firecrawl

| Item | Firecrawl | pagesift | Why |
| :--- | :--- | :--- | :--- |
| Error names | `BAD_REQUEST` / `UNSUPPORTED_FILE_TYPE` / `PARSE_UNSUPPORTED_OPTIONS` | `INVALID_REQUEST` / `UNSUPPORTED_OPTION` | Matches scrape's codes; the status is 400 either way |
| Over the limit | Not documented | 413 `TOO_LARGE` | Uses the existing code |
| Damaged PDF or document | 500 | 400 `INVALID_REQUEST` | The file is at fault, not the server |
| Encrypted PDF/Office, DRM EPUB | — | 400 `UNSUPPORTED_OPTION` | Needs a password or key |
| Type detection | Extension, then MIME | Content (`%PDF-`, parts inside the ZIP), then extension and MIME | A wrong file name still works |
| `mode: ocr` | Supported | Supported once `[ocr] enabled` is set, otherwise reported as unsupported | No silent downgrade |
| `blocks`, `pageMarkers` | Supported | Reported as unsupported | No silent downgrade |
| `mode: fast` on a scan | `SCRAPE_PDF_OCR_REQUIRED` | 400 `UNSUPPORTED_OPTION` | Same failure, existing code |
| `image` parser (image OCR) | Supported | Image files use OCR when it is on | Without OCR the request fails with a message saying so |
| `html` format for documents | HTML converted from Markdown | `markdown` only | No separately invented HTML |
| Document formats | Includes doc, xls, ppt, odt/ods/odp, rtf | Includes these and the formats listed at the top of this page | |
| Word headers and footers | Before and after the body | Not output | Content repeated on every page would mix into the body |
| Hidden rows and columns in Excel | Not verified (Firecrawl converts with anydoc; MarkItDown outputs them) | Not output | Users cannot see them in a spreadsheet |

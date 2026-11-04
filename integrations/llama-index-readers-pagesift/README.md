# llama-index-readers-pagesift

A LlamaIndex reader for [pagesift](https://github.com/YOLOW92/pagesift). It runs locally in the pagesift Rust core: no API key and no service to run.

```bash
pip install llama-index-readers-pagesift
```

```python
from llama_index.readers.pagesift import PagesiftReader

reader = PagesiftReader()

# Pages, fetched concurrently
docs = reader.load_data(urls=["https://example.com"])

# A whole site
docs = reader.load_data(crawl="https://example.com/docs/", limit=50, max_depth=3)

# Local files: PDF, Office, EPUB, email, archives, text formats
docs = reader.load_data(files=["report.pdf", "mail.eml", "data.zip"])
```

`lazy_load_data()`, `aload_data()` and `alazy_load_data()` take the same arguments. Give exactly one of `urls`, `crawl` or `files`. Other keyword arguments are pagesift scrape options, such as `only_main_content=False`.

| Option | Meaning |
|---|---|
| `limit`, `max_depth`, `include_paths`, `exclude_paths` | Limits for `crawl` |
| `scraper` (constructor) | Reuse a `pagesift.Scraper` (needed for settings such as `ocr=True`) |
| `raise_on_error` (constructor) | Raise on the first failed page instead of logging and skipping it |

Each document's metadata has `source`, then `title`, `description`, `language`, `author`, `published_time`, `content_type`, `status_code` and `content_hash` when known. `warnings` is present when pagesift says a result may be incomplete. `status_code`, `content_hash` and `warnings` stay out of embeddings and prompts.

License: AGPL-3.0, or a [commercial license](https://github.com/YOLOW92/pagesift/blob/main/docs/COMMERCIAL.md).

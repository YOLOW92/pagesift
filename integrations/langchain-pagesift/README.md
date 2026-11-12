# langchain-pagesift

A LangChain document loader for [pagesift](https://github.com/YOLOW92/pagesift). It runs locally in the pagesift Rust core: no API key and no service to run.

```bash
pip install langchain-pagesift
```

```python
from langchain_pagesift import PagesiftLoader

# Pages, fetched concurrently
docs = PagesiftLoader(urls=["https://example.com"]).load()

# A whole site
docs = PagesiftLoader(crawl="https://example.com/docs/", limit=50, max_depth=3).load()

# Local files: PDF, Office, EPUB, email, archives, text formats
docs = PagesiftLoader(files=["report.pdf", "mail.eml", "data.zip"]).load()
```

`lazy_load()` and `alazy_load()` yield each document as it finishes.

Give exactly one of `urls`, `crawl` or `files`. Other keyword arguments are pagesift scrape options, such as `only_main_content=False`.

| Option | Meaning |
|---|---|
| `limit`, `max_depth`, `include_paths`, `exclude_paths` | Limits for `crawl` |
| `scraper` | Reuse a `pagesift.Scraper` (needed for settings such as `ocr=True`) |
| `raise_on_error` | Raise on the first failed page instead of logging and skipping it |

Each document's metadata has `source`, then `title`, `description`, `language`, `author`, `published_time`, `content_type`, `status_code` and `content_hash` when known. `warnings` is present when pagesift says a result may be incomplete.

License: Apache-2.0.

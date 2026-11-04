"""LangChain document loader for pagesift: web pages, whole sites and local files as Markdown.

::

    from langchain_pagesift import PagesiftLoader

    PagesiftLoader(urls=["https://example.com"]).load()
    PagesiftLoader(crawl="https://example.com/docs/", limit=50).load()
    PagesiftLoader(files=["report.pdf", "mail.eml", "data.zip"]).load()

Everything runs locally in the pagesift Rust core; no API key and no service to run.
"""

from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterator, Dict, Iterable, Iterator, List, Optional, Sequence, Tuple, Union

import pagesift
from langchain_core.document_loaders import BaseLoader
from langchain_core.documents import Document

__all__ = ["PagesiftLoader"]

logger = logging.getLogger(__name__)

PathLike = Union[str, "os.PathLike[str]"]

_METADATA_KEYS = ("title", "description", "language", "author", "published_time", "content_type")


class PagesiftLoader(BaseLoader):
    """Load Markdown documents with pagesift.

    Give exactly one source:

    - ``urls``: pages to scrape, fetched concurrently;
    - ``crawl``: a start URL; follows links, limited by ``limit`` and ``max_depth``, and
      narrowed by ``include_paths`` and ``exclude_paths``;
    - ``files``: local files. Type is detected from the content, then the name.

    ``scraper`` reuses an existing :class:`pagesift.Scraper` (connection pools, rate limits,
    browsers); otherwise one is created and closed for each load. A page that fails is logged and
    skipped unless ``raise_on_error`` is true. Other keyword arguments are pagesift scrape options,
    such as ``only_main_content=False``.

    Each document's metadata has ``source`` (the URL or file name), the page's metadata that was
    found, and ``warnings`` as one string when pagesift says the result may be incomplete.
    """

    def __init__(
        self,
        urls: Optional[Union[str, Sequence[str]]] = None,
        *,
        crawl: Optional[str] = None,
        files: Optional[Union[PathLike, Sequence[PathLike]]] = None,
        limit: Optional[int] = None,
        max_depth: Optional[int] = None,
        include_paths: Optional[Sequence[str]] = None,
        exclude_paths: Optional[Sequence[str]] = None,
        scraper: Optional[pagesift.Scraper] = None,
        raise_on_error: bool = False,
        **options: Any,
    ) -> None:
        if sum(source is not None for source in (urls, crawl, files)) != 1:
            raise ValueError("give exactly one of urls, crawl or files")
        self.urls: Optional[List[str]] = [urls] if isinstance(urls, str) else (list(urls) if urls is not None else None)
        self.crawl = crawl
        if files is None:
            self.files: Optional[List[PathLike]] = None
        elif isinstance(files, (str, os.PathLike)):
            self.files = [files]
        else:
            self.files = list(files)
        self.crawl_options: Dict[str, Any] = {
            "limit": limit,
            "max_depth": max_depth,
            "include_paths": include_paths,
            "exclude_paths": exclude_paths,
        }
        self.scraper = scraper
        self.raise_on_error = raise_on_error
        self.options = options

    def _start(self, scraper: pagesift.Scraper) -> Any:
        """Start the work. The result can be iterated with ``for`` and ``async for``."""
        if self.urls is not None:
            return scraper.scrape_iter(self.urls, **self.options)
        if self.crawl is not None:
            return scraper.crawl(self.crawl, **self.crawl_options, **self.options)
        return scraper.parse_many(_read_files(self.files or []), **self.options)

    def _document(self, doc: Any) -> Optional[Document]:
        if not doc.success:
            if self.raise_on_error:
                raise doc.error or RuntimeError(doc.error_message or "pagesift failed")
            logger.warning("pagesift: %s: %s", doc.url, doc.error_message)
            return None
        return Document(page_content=doc.markdown or "", metadata=_metadata(doc))

    def lazy_load(self) -> Iterator[Document]:
        own = self.scraper is None
        scraper = self.scraper or pagesift.Scraper()
        try:
            for doc in self._start(scraper):
                out = self._document(doc)
                if out is not None:
                    yield out
        finally:
            if own:
                scraper.close()

    async def alazy_load(self) -> AsyncIterator[Document]:
        own = self.scraper is None
        scraper = self.scraper or pagesift.Scraper()
        try:
            async for doc in self._start(scraper):
                out = self._document(doc)
                if out is not None:
                    yield out
        finally:
            if own:
                await scraper.aclose()


def _read_files(paths: Iterable[PathLike]) -> Iterator[Tuple[bytes, str]]:
    for path in paths:
        with open(path, "rb") as f:
            yield f.read(), os.path.basename(os.fspath(path))


def _metadata(doc: Any) -> Dict[str, Any]:
    out: Dict[str, Any] = {"source": doc.final_url or doc.url}
    found = doc.metadata or {}
    for key in _METADATA_KEYS:
        value = found.get(key)
        if value not in (None, ""):
            out[key] = value
    if doc.status_code is not None:
        out["status_code"] = doc.status_code
    if doc.content_hash:
        out["content_hash"] = doc.content_hash
    if doc.warnings:
        # 向量库的 metadata 大多只收标量
        out["warnings"] = "; ".join(doc.warnings)
    return out

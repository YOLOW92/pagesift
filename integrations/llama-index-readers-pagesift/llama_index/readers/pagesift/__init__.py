"""LlamaIndex reader for pagesift: web pages, whole sites and local files as Markdown.

::

    from llama_index.readers.pagesift import PagesiftReader

    reader = PagesiftReader()
    reader.load_data(urls=["https://example.com"])
    reader.load_data(crawl="https://example.com/docs/", limit=50)
    reader.load_data(files=["report.pdf", "mail.eml", "data.zip"])

Everything runs locally in the pagesift Rust core; no API key and no service to run.
"""

from __future__ import annotations

import logging
import os
from typing import Any, AsyncIterable, Dict, Iterable, List, Optional, Sequence, Tuple, Union

import pagesift
from llama_index.core.readers.base import BaseReader
from llama_index.core.schema import Document

__all__ = ["PagesiftReader"]

logger = logging.getLogger(__name__)

PathLike = Union[str, "os.PathLike[str]"]

_METADATA_KEYS = ("title", "description", "language", "author", "published_time", "content_type")


class PagesiftReader(BaseReader):
    """Read Markdown documents with pagesift.

    ``scraper`` reuses an existing :class:`pagesift.Scraper` (connection pools, rate limits,
    browsers, settings such as ``ocr=True``); otherwise one is created and closed for each read.
    A page that fails is logged and skipped unless ``raise_on_error`` is true. ``options`` are
    pagesift scrape options applied to every read, such as ``only_main_content=False``.
    """

    def __init__(
        self,
        *,
        scraper: Optional[pagesift.Scraper] = None,
        raise_on_error: bool = False,
        **options: Any,
    ) -> None:
        self.scraper = scraper
        self.raise_on_error = raise_on_error
        self.options = options

    def lazy_load_data(
        self,
        urls: Optional[Union[str, Sequence[str]]] = None,
        *,
        crawl: Optional[str] = None,
        files: Optional[Union[PathLike, Sequence[PathLike]]] = None,
        limit: Optional[int] = None,
        max_depth: Optional[int] = None,
        include_paths: Optional[Sequence[str]] = None,
        exclude_paths: Optional[Sequence[str]] = None,
        **options: Any,
    ) -> Iterable[Document]:
        """Yield documents as they finish. Give exactly one of ``urls``, ``crawl`` or ``files``."""
        start = _starter(urls, crawl, files, limit, max_depth, include_paths, exclude_paths, {**self.options, **options})
        return self._iterate(start)

    def _iterate(self, start: Any) -> Iterable[Document]:
        own = self.scraper is None
        scraper = self.scraper or pagesift.Scraper()
        try:
            for doc in start(scraper):
                out = self._document(doc)
                if out is not None:
                    yield out
        finally:
            if own:
                scraper.close()

    async def alazy_load_data(self, *args: Any, **kwargs: Any) -> AsyncIterable[Document]:
        """Async version of :meth:`lazy_load_data`."""
        start = _starter(*_arguments(args, kwargs, self.options))
        own = self.scraper is None
        scraper = self.scraper or pagesift.Scraper()
        try:
            async for doc in start(scraper):
                out = self._document(doc)
                if out is not None:
                    yield out
        finally:
            if own:
                await scraper.aclose()

    def load_data(self, *args: Any, **kwargs: Any) -> List[Document]:
        """Read everything into a list. Takes the same arguments as :meth:`lazy_load_data`."""
        return list(self.lazy_load_data(*args, **kwargs))

    async def aload_data(self, *args: Any, **kwargs: Any) -> List[Document]:
        """Async version of :meth:`load_data`."""
        return [doc async for doc in self.alazy_load_data(*args, **kwargs)]

    def _document(self, doc: Any) -> Optional[Document]:
        if not doc.success:
            if self.raise_on_error:
                raise doc.error or RuntimeError(doc.error_message or "pagesift failed")
            logger.warning("pagesift: %s: %s", doc.url, doc.error_message)
            return None
        metadata = _metadata(doc)
        return Document(
            text=doc.markdown or "",
            metadata=metadata,
            # 不进嵌入和大模型提示：这些是管理用字段，不是内容
            excluded_embed_metadata_keys=[k for k in ("status_code", "content_hash", "warnings") if k in metadata],
            excluded_llm_metadata_keys=[k for k in ("status_code", "content_hash", "warnings") if k in metadata],
        )


def _arguments(args: Sequence[Any], kwargs: Dict[str, Any], defaults: Dict[str, Any]) -> Tuple[Any, ...]:
    urls = args[0] if args else kwargs.pop("urls", None)
    named = [kwargs.pop(k, None) for k in ("crawl", "files", "limit", "max_depth", "include_paths", "exclude_paths")]
    return (urls, *named, {**defaults, **kwargs})


def _starter(
    urls: Optional[Union[str, Sequence[str]]],
    crawl: Optional[str],
    files: Optional[Union[PathLike, Sequence[PathLike]]],
    limit: Optional[int],
    max_depth: Optional[int],
    include_paths: Optional[Sequence[str]],
    exclude_paths: Optional[Sequence[str]],
    options: Dict[str, Any],
) -> Any:
    """Check the arguments now and return the function that starts the work on a scraper."""
    if sum(source is not None for source in (urls, crawl, files)) != 1:
        raise ValueError("give exactly one of urls, crawl or files")
    if urls is not None:
        pages = [urls] if isinstance(urls, str) else list(urls)
        return lambda s: s.scrape_iter(pages, **options)
    if crawl is not None:
        limits = {
            "limit": limit,
            "max_depth": max_depth,
            "include_paths": include_paths,
            "exclude_paths": exclude_paths,
        }
        return lambda s: s.crawl(crawl, **limits, **options)
    paths = [files] if isinstance(files, (str, os.PathLike)) else list(files or [])
    return lambda s: s.parse_many(_read_files(paths), **options)


def _read_files(paths: Iterable[PathLike]) -> Iterable[Tuple[bytes, str]]:
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

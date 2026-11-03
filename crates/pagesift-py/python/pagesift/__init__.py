"""pagesift：把网页变成给大模型用的干净 Markdown，核心用 Rust 实现。

::

    import pagesift

    doc = pagesift.scrape("https://example.com")

    with pagesift.Scraper() as s:                      # 长期复用：连接池、浏览器池
        for doc in s.scrape_iter(urls):                # 批量，按完成顺序
            ...

    async for doc in pagesift.ascrape_many(urls):      # 异步批量，按完成顺序
        ...

    with pagesift.Scraper() as s:
        for doc in s.crawl("https://example.com/docs/", limit=50):   # 整站爬取，一页完成产出一页
            ...
        links = s.map("https://example.com")                        # 站点链接列表
        doc = s.parse(open("a.pdf", "rb").read(), "a.pdf")          # 解析本地文件

抓取和处理都在 Rust 里完成，等待期间释放 GIL；批量接口的并发在 Rust 侧完成；
Document 的字段访问时才转换成 Python 对象。
"""

from __future__ import annotations

import atexit
import dataclasses
import threading
from typing import Any, Dict, Iterable, List, Literal, Mapping, Optional, Sequence, Tuple, Union

from . import _pagesift
from ._pagesift import Batch, Crawl, Document, __version__, convert
from .errors import (
    AccessBarrier,
    Busy,
    DnsFailure,
    HostCooldown,
    InternalError,
    InvalidRequest,
    NetworkError,
    PagesiftError,
    RateLimited,
    RobotsDisallowed,
    SsrfBlocked,
    Timeout,
    TooLarge,
    UnsupportedOption,
    UpstreamError,
)

__all__ = [
    "__version__",
    "Batch",
    "Crawl",
    "Document",
    "ScrapeOptions",
    "Scraper",
    "ascrape",
    "ascrape_many",
    "convert",
    "default_scraper",
    "scrape",
    "scrape_iter",
    "scrape_many",
    # 异常
    "PagesiftError",
    "AccessBarrier",
    "Busy",
    "DnsFailure",
    "HostCooldown",
    "InternalError",
    "InvalidRequest",
    "NetworkError",
    "RateLimited",
    "RobotsDisallowed",
    "SsrfBlocked",
    "Timeout",
    "TooLarge",
    "UnsupportedOption",
    "UpstreamError",
]

Format = Literal["markdown", "html", "raw_html", "links", "images"]
EngineName = Literal["http", "browser", "auto"]
SitemapMode = Literal["include", "skip", "only"]
BytesLike = Union[bytes, bytearray, memoryview]

DEFAULT_CONCURRENCY = 16


@dataclasses.dataclass(frozen=True)
class ScrapeOptions:
    """抓取选项。创建时就校验，不合法立即抛异常；校验结果缓存在对象里，反复使用没有额外开销。

    - ``formats``：要返回的内容，默认只有 markdown；只请求需要的格式，其他的不计算；
    - ``only_main_content``：只保留正文，默认 True；
    - ``engine``：``http``、``browser`` 或 ``auto``（默认，先 HTTP，正文太少再用浏览器渲染）；
    - ``timeout``：总超时，毫秒；
    - ``wait_for``：浏览器渲染后再等多少毫秒；
    - ``headers``：额外的请求头；
    - ``include_tags``、``exclude_tags``：提取前按 CSS 选择器只保留／删掉匹配的元素（标签名、``.class``、``#id`` 等），选择器写错报 InvalidRequest；
    - ``ignore_robots_txt``：不检查 robots.txt；
    - ``mobile``：浏览器渲染时模拟手机；
    - ``languages``：浏览器渲染时的 Accept-Language，例如 ``["zh-CN", "en"]``。
    """

    formats: Sequence[Format] = ("markdown",)
    only_main_content: bool = True
    engine: EngineName = "auto"
    timeout: Optional[int] = None
    wait_for: int = 0
    headers: Optional[Mapping[str, str]] = None
    include_tags: Optional[Sequence[str]] = None
    exclude_tags: Optional[Sequence[str]] = None
    ignore_robots_txt: bool = False
    mobile: bool = False
    languages: Optional[Sequence[str]] = None
    proxy: Optional[str] = None
    if_none_match: Optional[str] = None
    if_modified_since: Optional[str] = None
    _native: _pagesift.Options = dataclasses.field(
        init=False, repr=False, compare=False, hash=False
    )

    def __post_init__(self) -> None:
        if isinstance(self.formats, str):
            object.__setattr__(self, "formats", (self.formats,))
        native = _pagesift.Options(
            formats=list(self.formats),
            only_main_content=self.only_main_content,
            engine=self.engine,
            timeout=self.timeout,
            wait_for=self.wait_for,
            headers=dict(self.headers) if self.headers else None,
            include_tags=list(self.include_tags) if self.include_tags else None,
            exclude_tags=list(self.exclude_tags) if self.exclude_tags else None,
            ignore_robots_txt=self.ignore_robots_txt,
            mobile=self.mobile,
            languages=list(self.languages) if self.languages else None,
            proxy=self.proxy,
            if_none_match=self.if_none_match,
            if_modified_since=self.if_modified_since,
        )
        object.__setattr__(self, "_native", native)

    def replace(self, **changes: Any) -> "ScrapeOptions":
        """返回改了部分选项的新对象。不认识的选项名全部列出后抛 TypeError，不悄悄忽略。"""
        known = {f.name for f in dataclasses.fields(self) if f.init}
        unknown = [k for k in changes if k not in known]
        if unknown:
            raise TypeError(
                "unknown option"
                + ("s " if len(unknown) > 1 else " ")
                + ", ".join(repr(k) for k in unknown)
            )
        return dataclasses.replace(self, **changes)


_DEFAULT_OPTIONS = ScrapeOptions()


def _native(options: Optional[ScrapeOptions], overrides: Mapping[str, Any]) -> _pagesift.Options:
    if options is not None and not isinstance(options, ScrapeOptions):
        raise TypeError(
            f"options must be a pagesift.ScrapeOptions, got {type(options).__name__}"
        )
    base = options or _DEFAULT_OPTIONS
    return (base.replace(**overrides) if overrides else base)._native


def _bytes(data: BytesLike) -> bytes:
    if isinstance(data, bytes):
        return data
    if isinstance(data, (bytearray, memoryview)):
        return bytes(data)
    raise TypeError(f"data must be bytes, got {type(data).__name__}")


def _urls(urls: Iterable[str]) -> List[str]:
    if isinstance(urls, str):
        raise TypeError("urls must be an iterable of URLs, not a single string")
    return urls if isinstance(urls, list) else list(urls)


class Scraper:
    """抓取器：持有连接池、礼貌策略状态（robots.txt 缓存、限速、冷却）和浏览器池。

    创建一次、长期复用最快。用 ``with`` / ``async with`` 管理生命周期，或手动 ``close()`` / ``aclose()``。

    - ``user_agent``：自定义 UA（不允许冒充浏览器）；
    - ``allow_private_network``：允许抓内网和本机地址（默认拒绝，防 SSRF）；
    - ``request_interval``：同一主机两次请求之间的最小间隔，秒；robots.txt 的 Crawl-delay 优先；
    - ``per_host_concurrency``、``max_concurrency``：单主机和全局的并发上限；
    - ``browser_executable``、``browser_url``：指定 Chrome 路径，或连接已经在运行的浏览器
      （``ws://…`` / ``http://host:port``）；
    - ``max_browsers``：浏览器池里最多几个浏览器进程。

    所有方法都接受一个可选的 :class:`ScrapeOptions`，再加上覆盖其中字段的关键字参数。
    """

    def __init__(
        self,
        *,
        user_agent: Optional[str] = None,
        allow_private_network: bool = False,
        allow_cidrs: Optional[Sequence[str]] = None,
        proxy: Optional[str] = None,
        request_interval: float = 1.0,
        per_host_concurrency: int = 2,
        max_concurrency: int = 64,
        browser_executable: Optional[str] = None,
        browser_url: Optional[str] = None,
        max_browsers: int = 2,
    ) -> None:
        self._engine = _pagesift.Engine(
            user_agent=user_agent,
            allow_private_network=allow_private_network,
            allow_cidrs=list(allow_cidrs) if allow_cidrs else None,
            proxy=proxy,
            request_interval=request_interval,
            per_host_concurrency=per_host_concurrency,
            max_concurrency=max_concurrency,
            browser_executable=browser_executable,
            browser_url=browser_url,
            max_browsers=max_browsers,
        )

    def scrape(self, url: str, options: Optional[ScrapeOptions] = None, **overrides: Any) -> Document:
        """抓一个页面。失败时抛出 :class:`PagesiftError` 的子类。"""
        return self._engine.scrape(url, _native(options, overrides))

    async def ascrape(
        self, url: str, options: Optional[ScrapeOptions] = None, **overrides: Any
    ) -> Document:
        """``scrape`` 的异步版本。"""
        return await self._engine.ascrape(url, _native(options, overrides))

    def scrape_many(
        self,
        urls: Iterable[str],
        options: Optional[ScrapeOptions] = None,
        *,
        concurrency: int = DEFAULT_CONCURRENCY,
        **overrides: Any,
    ) -> List[Document]:
        """批量抓取，全部完成后按输入顺序返回。

        单个失败不抛异常：对应项 ``success`` 为 False，``error`` 是异常实例。
        """
        return self._engine.scrape_many(_urls(urls), _native(options, overrides), concurrency)

    def scrape_iter(
        self,
        urls: Iterable[str],
        options: Optional[ScrapeOptions] = None,
        *,
        concurrency: int = DEFAULT_CONCURRENCY,
        **overrides: Any,
    ) -> Batch:
        """批量抓取，按完成顺序产出：``for doc in scraper.scrape_iter(urls)``。

        调用时立即开始；``doc.index`` 是它在输入里的位置。等待期间释放 GIL。
        """
        return self._engine.stream(_urls(urls), _native(options, overrides), concurrency)

    def ascrape_many(
        self,
        urls: Iterable[str],
        options: Optional[ScrapeOptions] = None,
        *,
        concurrency: int = DEFAULT_CONCURRENCY,
        **overrides: Any,
    ) -> Batch:
        """异步批量抓取，按完成顺序产出：``async for doc in scraper.ascrape_many(urls)``。

        调用时立即开始；``batch.cancel()`` 或丢弃迭代器会取消未完成的抓取（``cancel()`` 之后迭代立即结束，
        已完成但还没取出的结果也丢弃）。
        """
        return self._engine.stream(_urls(urls), _native(options, overrides), concurrency)

    def crawl(
        self,
        url: str,
        options: Optional[ScrapeOptions] = None,
        *,
        limit: Optional[int] = None,
        max_depth: Optional[int] = None,
        include_paths: Optional[Sequence[str]] = None,
        exclude_paths: Optional[Sequence[str]] = None,
        crawl_entire_domain: bool = False,
        allow_subdomains: Optional[bool] = None,
        sitemap: SitemapMode = "include",
        query: Optional[str] = None,
        adaptive: bool = False,
        max_concurrency: Optional[int] = None,
        **overrides: Any,
    ) -> Crawl:
        """整站爬取，一页完成产出一页：``for doc in scraper.crawl(url, limit=50)``（``async for`` 也可以）。

        - ``limit``：最多抓多少页（不给时和 HTTP 接口的默认值一样）；带 ``adaptive`` 时只是上限；
        - ``max_depth``：从起始页算起最多跟几层链接；
        - ``include_paths``、``exclude_paths``：按路径的正则只抓／不抓匹配的页面；
        - ``crawl_entire_domain``：不限于起始页所在目录，整个主机都爬；
        - ``allow_subdomains``：也爬同一站点的子域名；
        - ``sitemap``：``include``（默认，sitemap 和页面链接都用）、``skip``（不读 sitemap）、``only``（只抓 sitemap 里的）；
        - ``query``：按和查询的相关性优先爬取；``adaptive``：内容覆盖查询后自动停止（需要 ``query``）；
        - ``max_concurrency``：这次爬取同时在抓的页面数上限；
        - 其余关键字参数和 ``options`` 是每页的抓取选项；其中 ``ignore_robots_txt`` 也作用于整个爬取。

        调用时立即开始。失败页也会产出（``success`` 为 False），爬取本身出错（比如路径正则写错）时迭代抛异常。
        ``break`` 或 ``crawl.cancel()`` 停止爬取；结束后 ``crawl.summary`` 给出停止原因。
        消费方慢时爬取在交付处等待，不丢页。
        """
        return self._engine.crawl(
            url,
            _native(options, overrides),
            limit=limit,
            max_depth=max_depth,
            include_paths=list(include_paths) if include_paths else None,
            exclude_paths=list(exclude_paths) if exclude_paths else None,
            crawl_entire_domain=crawl_entire_domain,
            allow_subdomains=allow_subdomains,
            sitemap=sitemap,
            query=query,
            adaptive=adaptive,
            max_concurrency=max_concurrency,
        )

    def map(
        self,
        url: str,
        *,
        limit: Optional[int] = None,
        search: Optional[str] = None,
        sitemap: SitemapMode = "include",
        ignore_robots_txt: bool = False,
    ) -> List[Dict[str, Optional[str]]]:
        """站点的链接列表：每项是 ``{"url", "title", "description"}``（``title``、``description`` 可能为 None）。

        ``search``：地址里命中搜索词多的链接排在前面，没命中的保留在后面；``sitemap`` 的取值同 :meth:`crawl`。
        """
        return self._engine.map(
            url, limit=limit, search=search, sitemap=sitemap, ignore_robots_txt=ignore_robots_txt
        )

    async def amap(
        self,
        url: str,
        *,
        limit: Optional[int] = None,
        search: Optional[str] = None,
        sitemap: SitemapMode = "include",
        ignore_robots_txt: bool = False,
    ) -> List[Dict[str, Optional[str]]]:
        """``map`` 的异步版本。"""
        return await self._engine.amap(
            url, limit=limit, search=search, sitemap=sitemap, ignore_robots_txt=ignore_robots_txt
        )

    def parse(
        self,
        data: BytesLike,
        filename: str,
        options: Optional[ScrapeOptions] = None,
        *,
        content_type: Optional[str] = None,
        url: Optional[str] = None,
        **overrides: Any,
    ) -> Document:
        """解析一个文件：PDF、HTML、Word、Excel、PowerPoint、EPUB、CSV。结果和 :meth:`scrape` 一样。

        类型按内容识别，认不出时再看 ``filename`` 的扩展名和 ``content_type``；仍认不出抛 UnsupportedOption。
        ``url`` 是页面原地址（HTML 的相对链接按它解析），不会去抓。不支持浏览器渲染相关的选项。
        """
        return self._engine.parse(
            _bytes(data), filename, _native(options, overrides), content_type=content_type, url=url
        )

    async def aparse(
        self,
        data: BytesLike,
        filename: str,
        options: Optional[ScrapeOptions] = None,
        *,
        content_type: Optional[str] = None,
        url: Optional[str] = None,
        **overrides: Any,
    ) -> Document:
        """``parse`` 的异步版本。"""
        return await self._engine.aparse(
            _bytes(data), filename, _native(options, overrides), content_type=content_type, url=url
        )

    def parse_many(
        self,
        files: Iterable[Tuple[BytesLike, str]],
        options: Optional[ScrapeOptions] = None,
        *,
        concurrency: int = DEFAULT_CONCURRENCY,
        **overrides: Any,
    ) -> Batch:
        """批量解析 ``(字节, 文件名)``，按完成顺序产出：``for doc in scraper.parse_many(files)``（``async for`` 也可以）。

        调用时立即开始；``doc.index`` 是它在输入里的位置，``doc.url`` 是文件名。单个失败不抛异常。
        取消语义和 :meth:`ascrape_many` 一样。
        """
        if isinstance(files, (bytes, str)):
            raise TypeError("files must be an iterable of (data, filename) pairs")
        items = [(_bytes(data), name) for data, name in files]
        return self._engine.parse_stream(items, _native(options, overrides), concurrency)

    def close(self) -> None:
        """关闭浏览器池。可以重复调用。"""
        self._engine.close()

    async def aclose(self) -> None:
        await self._engine.aclose()

    def __enter__(self) -> "Scraper":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    async def __aenter__(self) -> "Scraper":
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.aclose()


_default_lock = threading.Lock()
_default_scraper: Optional[Scraper] = None


def default_scraper() -> Scraper:
    """模块级函数共用的 Scraper（第一次用时创建，进程退出时关闭）。"""
    global _default_scraper
    if _default_scraper is None:
        with _default_lock:
            if _default_scraper is None:
                _default_scraper = Scraper()
                atexit.register(_default_scraper.close)
    return _default_scraper


def scrape(url: str, options: Optional[ScrapeOptions] = None, **overrides: Any) -> Document:
    """抓一个页面。失败时抛出 :class:`PagesiftError` 的子类。"""
    return default_scraper().scrape(url, options, **overrides)


async def ascrape(url: str, options: Optional[ScrapeOptions] = None, **overrides: Any) -> Document:
    """``scrape`` 的异步版本。"""
    return await default_scraper().ascrape(url, options, **overrides)


def scrape_many(
    urls: Iterable[str],
    options: Optional[ScrapeOptions] = None,
    *,
    concurrency: int = DEFAULT_CONCURRENCY,
    request_interval: Optional[float] = None,
    per_host_concurrency: Optional[int] = None,
    max_concurrency: Optional[int] = None,
    **overrides: Any,
) -> List[Document]:
    """批量抓取，全部完成后按输入顺序返回；单个失败不影响整批。

    ``request_interval``、``per_host_concurrency`` 和 ``max_concurrency``
    配置这一次批量使用的临时抓取器。robots.txt 的 Crawl-delay 仍优先于
    ``request_interval``；``concurrency`` 只控制这批同时在途的任务数。
    """
    if any(v is not None for v in (request_interval, per_host_concurrency, max_concurrency)):
        config = {}
        if request_interval is not None:
            config["request_interval"] = request_interval
        if per_host_concurrency is not None:
            config["per_host_concurrency"] = per_host_concurrency
        if max_concurrency is not None:
            config["max_concurrency"] = max_concurrency
        with Scraper(**config) as scraper:
            return scraper.scrape_many(
                urls, options, concurrency=concurrency, **overrides
            )
    return default_scraper().scrape_many(urls, options, concurrency=concurrency, **overrides)


def scrape_iter(
    urls: Iterable[str],
    options: Optional[ScrapeOptions] = None,
    *,
    concurrency: int = DEFAULT_CONCURRENCY,
    **overrides: Any,
) -> Batch:
    """批量抓取，按完成顺序产出：``for doc in pagesift.scrape_iter(urls)``。"""
    return default_scraper().scrape_iter(urls, options, concurrency=concurrency, **overrides)


def ascrape_many(
    urls: Iterable[str],
    options: Optional[ScrapeOptions] = None,
    *,
    concurrency: int = DEFAULT_CONCURRENCY,
    **overrides: Any,
) -> Batch:
    """异步批量抓取，按完成顺序产出：``async for doc in pagesift.ascrape_many(urls)``。"""
    return default_scraper().ascrape_many(urls, options, concurrency=concurrency, **overrides)

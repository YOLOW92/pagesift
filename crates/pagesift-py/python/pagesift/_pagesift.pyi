# pagesift 原生模块（Rust）的类型存根。公开接口见 pagesift/__init__.py。

from typing import Any, Awaitable, Dict, Iterator, List, Mapping, Optional, Sequence, Tuple, TypedDict

from .errors import PagesiftError

__version__: str

class Document:
    """抓取或转换的结果。失败的批量项也是 Document：``success`` 为 False，``error`` 是异常实例。

    字段第一次访问时才从 Rust 转换成 Python 对象，字符串字段转换后缓存。
    """

    @property
    def index(self) -> Optional[int]:
        """在批量输入里的位置；单页抓取和 ``convert`` 为 None。"""

    @property
    def url(self) -> Optional[str]:
        """请求的地址；``parse`` 的结果是文件名。"""
    @property
    def final_url(self) -> Optional[str]:
        """跟随重定向后的最终地址。"""
    @property
    def success(self) -> bool: ...
    @property
    def status_code(self) -> Optional[int]: ...
    @property
    def markdown(self) -> Optional[str]: ...
    @property
    def html(self) -> Optional[str]:
        """清理后的 HTML（``formats`` 里有 ``html`` 时才有）。"""
    @property
    def raw_html(self) -> Optional[str]:
        """原始 HTML（``formats`` 里有 ``raw_html`` 时才有）。"""
    @property
    def links(self) -> Optional[List[str]]: ...
    @property
    def images(self) -> Optional[List[str]]: ...
    @property
    def extract(self) -> Optional[List[Dict[str, Any]]]:
        """``extract_schema`` 抽出的列表（每个列表项一个对象）；没要时为 None。"""
    @property
    def metadata(self) -> Dict[str, Any]:
        """页面元数据：title、description、language、keywords、robots、favicon、
        published_time、author、modified_time、content_type，以及 og_image 等其他 meta 标签（scrape 和 convert 键名相同）。"""
    @property
    def error(self) -> Optional[PagesiftError]: ...
    @property
    def error_message(self) -> Optional[str]: ...
    @property
    def error_code(self) -> Optional[str]: ...
    @property
    def engine(self) -> Optional[str]:
        """实际使用的引擎：``http`` 或 ``browser``。"""
    @property
    def content_hash(self) -> Optional[str]:
        """Markdown 的 BLAKE3 哈希（十六进制）。"""
    @property
    def raw_hash(self) -> Optional[str]: ...
    @property
    def text_hash(self) -> Optional[str]: ...
    @property
    def elapsed_ms(self) -> Optional[float]: ...
    @property
    def timings(self) -> Dict[str, float]:
        """各阶段耗时（毫秒）：fetch、render、extract、pdf、total。"""
    @property
    def warnings(self) -> List[str]: ...
    @property
    def ocr(self) -> Optional[Dict[str, Any]]: ...
    @property
    def redirect_chain(self) -> List[str]: ...
    @property
    def etag(self) -> Optional[str]: ...
    @property
    def last_modified(self) -> Optional[str]: ...
    @property
    def not_modified(self) -> bool: ...
    def to_dict(self) -> Dict[str, Any]:
        """全部字段转成 dict（会转换所有字段，只要部分字段时直接访问属性更快）。"""

def convert(html: str, url: Optional[str] = None, only_main_content: bool = True) -> Document:
    """把一段 HTML 转成干净的 Markdown，处理期间释放 GIL。"""

class Options:
    def __init__(
        self,
        *,
        formats: Optional[Sequence[str]] = None,
        only_main_content: bool = True,
        engine: Optional[str] = None,
        timeout: Optional[int] = None,
        wait_for: int = 0,
        headers: Optional[Mapping[str, str]] = None,
        include_tags: Optional[Sequence[str]] = None,
        exclude_tags: Optional[Sequence[str]] = None,
        ignore_robots_txt: bool = False,
        mobile: bool = False,
        languages: Optional[Sequence[str]] = None,
        proxy: Optional[str] = None,
        if_none_match: Optional[str] = None,
        if_modified_since: Optional[str] = None,
        export_images: bool = False,
        extract_schema: Optional[str] = None,
        max_age: Optional[int] = None,
        parsers: Optional[str] = None,
    ) -> None: ...

class Batch:
    """进行中的批量抓取或批量解析，按完成顺序产出 Document。``for`` 和 ``async for`` 都可以用，只能迭代一遍。"""

    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[Document]: ...
    def __next__(self) -> Document: ...
    def __aiter__(self) -> "Batch": ...
    def __anext__(self) -> Awaitable[Document]: ...
    def cancel(self) -> None:
        """取消整批：未完成的抓取全部取消，之后迭代立即结束；已完成但还没取出的结果也丢弃。"""

class CrawlSummary(TypedDict):
    stop_reason: str
    """``limit_reached``、``exhausted``、``cancelled``；带 query 时还有 ``coverage_reached``、``saturated``、``no_relevant_pages``。"""
    pages: int
    coverage: Optional[float]
    """自适应爬取结束时的覆盖度（0–1），其他爬取为 None。"""
    term_evidence: Dict[str, int]
    """自适应爬取里每个查询词收集到的证据块数，其他爬取为空。"""

class MapLink(TypedDict):
    url: str
    title: Optional[str]
    description: Optional[str]

class Crawl:
    """进行中的整站爬取，每完成一页产出一个 Document（失败页 ``success`` 为 False）。

    ``for`` 和 ``async for`` 都可以用，只能迭代一遍；爬取本身出错（比如路径正则写错）时迭代抛出异常。
    迭代器被丢弃或 ``cancel()`` 后爬取停止。
    """

    def __iter__(self) -> Iterator[Document]: ...
    def __next__(self) -> Document: ...
    def __aiter__(self) -> "Crawl": ...
    def __anext__(self) -> Awaitable[Document]: ...
    def cancel(self) -> None:
        """停止爬取，之后迭代立即结束；已完成但还没取出的页面也丢弃。"""
    @property
    def total(self) -> int:
        """已经确定要抓的页面数（随着发现新链接增长，不超过 ``limit``）。"""
    @property
    def pages(self) -> int:
        """引擎已经交付的成功页数（含还没取出的）。"""
    @property
    def errors(self) -> int:
        """引擎已经交付的失败页数（含还没取出的）。"""
    @property
    def summary(self) -> Optional[CrawlSummary]:
        """爬取结束后的说明，结束前为 None。"""

class Engine:
    def __init__(
        self,
        *,
        user_agent: Optional[str] = None,
        allow_private_network: bool = False,
        allow_cidrs: Optional[Sequence[str]] = None,
        proxy: Optional[str] = None,
        request_interval_ms: int = 1000,
        per_host_concurrency: int = 2,
        max_concurrency: int = 64,
        browser_executable: Optional[str] = None,
        browser_url: Optional[str] = None,
        max_browsers: int = 2,
        store_path: Optional[str] = None,
        ocr: bool = False,
        ocr_quality: Optional[str] = None,
        ocr_formulas: bool = False,
        models_dir: Optional[str] = None,
        gpu: Optional[str] = None,
    ) -> None: ...
    def scrape(self, url: str, options: Options) -> Document: ...
    def ascrape(self, url: str, options: Options) -> Awaitable[Document]: ...
    def scrape_many(self, urls: List[str], options: Options, concurrency: int) -> List[Document]: ...
    def stream(self, urls: List[str], options: Options, concurrency: int) -> Batch: ...
    def crawl(
        self,
        url: str,
        options: Options,
        *,
        limit: Optional[int] = None,
        max_depth: Optional[int] = None,
        include_paths: Optional[List[str]] = None,
        exclude_paths: Optional[List[str]] = None,
        crawl_entire_domain: bool = False,
        allow_subdomains: Optional[bool] = None,
        sitemap: str = "include",
        query: Optional[str] = None,
        adaptive: bool = False,
        max_concurrency: Optional[int] = None,
    ) -> Crawl: ...
    def map(
        self,
        url: str,
        *,
        limit: Optional[int] = None,
        search: Optional[str] = None,
        sitemap: str = "include",
        ignore_robots_txt: bool = False,
    ) -> List[MapLink]: ...
    def amap(
        self,
        url: str,
        *,
        limit: Optional[int] = None,
        search: Optional[str] = None,
        sitemap: str = "include",
        ignore_robots_txt: bool = False,
    ) -> Awaitable[List[MapLink]]: ...
    def parse(
        self,
        data: bytes,
        filename: str,
        options: Options,
        *,
        content_type: Optional[str] = None,
        url: Optional[str] = None,
    ) -> Document: ...
    def aparse(
        self,
        data: bytes,
        filename: str,
        options: Options,
        *,
        content_type: Optional[str] = None,
        url: Optional[str] = None,
    ) -> Awaitable[Document]: ...
    def parse_stream(self, files: List[Tuple[bytes, str]], options: Options, concurrency: int) -> Batch: ...
    def close(self) -> None: ...
    def aclose(self) -> Awaitable[None]: ...

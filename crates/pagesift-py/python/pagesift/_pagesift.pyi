# pagesift 原生模块（Rust）的类型存根。公开接口见 pagesift/__init__.py。

from typing import Any, Awaitable, Dict, Iterator, List, Mapping, Optional, Sequence

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
        """请求的地址。"""
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
    def metadata(self) -> Dict[str, Any]:
        """页面元数据：title、description、language、keywords、robots、favicon、
        published_time、modified_time、content_type，以及 og_image 等其他 meta 标签。"""
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
    def content_sha256(self) -> Optional[str]:
        """Markdown 的 SHA-256。"""
    @property
    def raw_sha256(self) -> Optional[str]: ...
    @property
    def text_sha256(self) -> Optional[str]: ...
    @property
    def elapsed_ms(self) -> Optional[float]: ...
    @property
    def timings(self) -> Dict[str, float]:
        """各阶段耗时（毫秒）：fetch、render、extract、pdf、total。"""
    @property
    def warnings(self) -> List[str]: ...
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
    ) -> None: ...

class Batch:
    """进行中的批量抓取，按完成顺序产出 Document。``for`` 和 ``async for`` 都可以用，只能迭代一遍。"""

    def __len__(self) -> int: ...
    def __iter__(self) -> Iterator[Document]: ...
    def __next__(self) -> Document: ...
    def __aiter__(self) -> "Batch": ...
    def __anext__(self) -> Awaitable[Document]: ...
    def cancel(self) -> None:
        """取消还没完成的抓取；已经完成的结果仍然可以取出。"""

class Engine:
    def __init__(
        self,
        *,
        user_agent: Optional[str] = None,
        allow_private_network: bool = False,
        request_interval: float = 1.0,
        per_host_concurrency: int = 2,
        max_concurrency: int = 64,
        browser_executable: Optional[str] = None,
        browser_url: Optional[str] = None,
        max_browsers: int = 2,
    ) -> None: ...
    def scrape(self, url: str, options: Options) -> Document: ...
    def ascrape(self, url: str, options: Options) -> Awaitable[Document]: ...
    def scrape_many(self, urls: List[str], options: Options, concurrency: int) -> List[Document]: ...
    def stream(self, urls: List[str], options: Options, concurrency: int) -> Batch: ...
    def close(self) -> None: ...
    def aclose(self) -> Awaitable[None]: ...

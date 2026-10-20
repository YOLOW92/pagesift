"""pagesift 的异常，和 HTTP 接口的错误码一一对应。

每个异常都带 ``code``（错误码字符串）、``message``、``details``（dict）、
``retry_after``（秒，可能为 None）和 ``url``。
"""

from __future__ import annotations

from typing import Any, Dict, Optional, Type

__all__ = [
    "PagesiftError",
    "RobotsDisallowed",
    "AccessBarrier",
    "RateLimited",
    "HostCooldown",
    "Timeout",
    "SsrfBlocked",
    "TooLarge",
    "UpstreamError",
    "UnsupportedOption",
    "InvalidRequest",
    "InternalError",
]


class PagesiftError(Exception):
    """所有 pagesift 异常的基类。"""

    code: str = "INTERNAL_ERROR"

    def __init__(
        self,
        message: str,
        *,
        code: Optional[str] = None,
        details: Optional[Dict[str, Any]] = None,
        retry_after: Optional[int] = None,
        url: Optional[str] = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.code = code or type(self).code
        self.details: Dict[str, Any] = details or {}
        self.retry_after = retry_after
        self.url = url

    def __str__(self) -> str:
        return f"{self.code}: {self.message}"

    def __reduce__(self):  # type: ignore[override]
        return (
            _from_code,
            (self.code, self.message),
            {"details": self.details, "retry_after": self.retry_after, "url": self.url},
        )

    def __setstate__(self, state: Dict[str, Any]) -> None:
        self.__dict__.update(state)


class RobotsDisallowed(PagesiftError):
    """robots.txt 禁止抓取这个地址。"""

    code = "ROBOTS_DISALLOWED"


class AccessBarrier(PagesiftError):
    """站点返回了拦截页（验证码、JS 挑战、登录墙等）；pagesift 不会绕过。

    ``details`` 里有 ``vendor``、``kind``、``confidence``、``ruleId``。
    """

    code = "ACCESS_BARRIER"


class RateLimited(PagesiftError):
    """目标站点返回 429。"""

    code = "RATE_LIMITED"


class HostCooldown(PagesiftError):
    """这个主机最近连续失败或限流，正在冷却，``retry_after`` 秒后再试。"""

    code = "HOST_COOLDOWN"


class Timeout(PagesiftError, TimeoutError):
    """连接、首字节或总时长超时。同时是内置 ``TimeoutError`` 的子类。"""

    code = "TIMEOUT"


class SsrfBlocked(PagesiftError):
    """目标地址是内网、回环等非公网地址。"""

    code = "SSRF_BLOCKED"


class TooLarge(PagesiftError):
    """响应超过大小上限。"""

    code = "TOO_LARGE"


class UpstreamError(PagesiftError):
    """目标站点出错（连接失败、TLS 错误、无法解析的内容、渲染失败等）。"""

    code = "UPSTREAM_ERROR"


class UnsupportedOption(PagesiftError, ValueError):
    """请求了这个版本做不到的事（比如不支持的格式）。pagesift 不会悄悄忽略。"""

    code = "UNSUPPORTED_OPTION"


class InvalidRequest(PagesiftError, ValueError):
    """参数不合法（地址、超时等）。同时是内置 ``ValueError`` 的子类。"""

    code = "INVALID_REQUEST"


class InternalError(PagesiftError):
    """pagesift 内部错误，请报告问题。"""

    code = "INTERNAL_ERROR"


ERRORS: Dict[str, Type[PagesiftError]] = {
    cls.code: cls
    for cls in (
        RobotsDisallowed,
        AccessBarrier,
        RateLimited,
        HostCooldown,
        Timeout,
        SsrfBlocked,
        TooLarge,
        UpstreamError,
        UnsupportedOption,
        InvalidRequest,
        InternalError,
    )
}
"""错误码 → 异常类。"""


def _from_code(
    code: str,
    message: str,
    *,
    details: Optional[Dict[str, Any]] = None,
    retry_after: Optional[int] = None,
    url: Optional[str] = None,
) -> PagesiftError:
    cls = ERRORS.get(code, PagesiftError)
    return cls(message, code=code, details=details, retry_after=retry_after, url=url)

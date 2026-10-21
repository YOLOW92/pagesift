"""错误码 → 异常类，批量接口里单个失败不影响整批。"""

import pickle

import pytest

import pagesift
from pagesift.errors import ERRORS, _from_code

CODES = {
    "ROBOTS_DISALLOWED": pagesift.RobotsDisallowed,
    "ACCESS_BARRIER": pagesift.AccessBarrier,
    "RATE_LIMITED": pagesift.RateLimited,
    "HOST_COOLDOWN": pagesift.HostCooldown,
    "TIMEOUT": pagesift.Timeout,
    "SSRF_BLOCKED": pagesift.SsrfBlocked,
    "TOO_LARGE": pagesift.TooLarge,
    "UPSTREAM_ERROR": pagesift.UpstreamError,
    "NETWORK_ERROR": pagesift.NetworkError,
    "DNS_FAILURE": pagesift.DnsFailure,
    "BUSY": pagesift.Busy,
    "UNSUPPORTED_OPTION": pagesift.UnsupportedOption,
    "INVALID_REQUEST": pagesift.InvalidRequest,
    "INTERNAL_ERROR": pagesift.InternalError,
}


@pytest.mark.parametrize("code, cls", sorted(CODES.items()))
def test_every_code_has_a_class(code, cls):
    assert ERRORS[code] is cls and cls.code == code
    e = _from_code(code, "msg", details={"a": 1}, retry_after=3, url="https://x/")
    assert type(e) is cls and isinstance(e, pagesift.PagesiftError)
    assert (e.code, e.message, e.details, e.retry_after, e.url) == (code, "msg", {"a": 1}, 3, "https://x/")
    assert str(e) == f"{code}: msg"
    again = pickle.loads(pickle.dumps(e))
    assert type(again) is cls and again.details == {"a": 1} and again.url == "https://x/"


def test_unknown_code_falls_back_to_base():
    e = _from_code("SOMETHING_NEW", "m")
    assert type(e) is pagesift.PagesiftError and e.code == "SOMETHING_NEW"


def test_builtin_bases():
    assert issubclass(pagesift.Timeout, TimeoutError)
    assert issubclass(pagesift.InvalidRequest, ValueError)
    assert issubclass(pagesift.UnsupportedOption, ValueError)
    # 新的细分错误仍然能被捕获 UpstreamError 的旧代码接住
    assert issubclass(pagesift.NetworkError, pagesift.UpstreamError)
    assert issubclass(pagesift.DnsFailure, pagesift.UpstreamError)


def test_robots_disallowed(scraper, http, server):
    with pytest.raises(pagesift.RobotsDisallowed) as e:
        scraper.scrape(f"{server}/private/page", http)
    assert e.value.code == "ROBOTS_DISALLOWED"
    assert e.value.url == f"{server}/private/page"
    # 单页抓取可以显式忽略 robots.txt
    assert scraper.scrape(f"{server}/private/page", http, ignore_robots_txt=True).success


def test_access_barrier_is_reported_not_bypassed(scraper, http, server):
    with pytest.raises(pagesift.AccessBarrier) as e:
        scraper.scrape(f"{server}/challenge", http)
    assert e.value.details["vendor"] == "cloudflare"
    assert e.value.details["kind"] == "challenge"
    assert e.value.details["confidence"] > 0.9


def test_rate_limited_then_cooldown(scraper, http, server):
    with pytest.raises(pagesift.RateLimited) as e:
        scraper.scrape(f"{server}/ratelimit", http)
    assert e.value.retry_after == 30
    # 主机进入冷却：其他页面也不再请求
    with pytest.raises(pagesift.HostCooldown) as e:
        scraper.scrape(f"{server}/article", http)
    assert 0 < e.value.retry_after <= 30


def test_timeout(scraper, http, server):
    with pytest.raises(pagesift.Timeout) as e:
        scraper.scrape(f"{server}/slow?ms=3000", http, timeout=1000)
    assert isinstance(e.value, TimeoutError)


def test_ssrf_blocked(server, http):
    with pagesift.Scraper() as s:
        with pytest.raises(pagesift.SsrfBlocked):
            s.scrape(f"{server}/article", http)


def test_upstream_error(scraper, http, closed_port):
    with pytest.raises(pagesift.UpstreamError):
        scraper.scrape(f"http://127.0.0.1:{closed_port}/", http)


def test_batch_partial_failure(scraper, http, server, closed_port):
    urls = [
        f"{server}/page/1",
        f"{server}/private/x",
        "ftp://nope/",
        f"{server}/challenge",
        f"http://127.0.0.1:{closed_port}/",
        f"{server}/page/2",
    ]
    docs = scraper.scrape_many(urls, http, concurrency=4)
    assert [d.url for d in docs] == urls
    assert [d.index for d in docs] == list(range(len(urls)))
    assert [d.success for d in docs] == [True, False, False, False, False, True]
    assert [type(d.error) for d in docs] == [
        type(None),
        pagesift.RobotsDisallowed,
        pagesift.InvalidRequest,
        pagesift.AccessBarrier,
        pagesift.UpstreamError,
        type(None),
    ]
    bad = docs[3]
    assert bad.error_code == "ACCESS_BARRIER" and bad.error_message and bad.markdown is None
    assert bad.error.url == urls[3]
    assert bad.error is bad.error  # 异常对象访问时才创建，之后复用
    d = bad.to_dict()
    assert d["success"] is False
    assert d["error"]["code"] == "ACCESS_BARRIER" and d["error"]["details"]["vendor"] == "cloudflare"
    assert d["error"]["message"] == bad.error_message
    assert "Page /page/2" in docs[5].markdown


def test_batch_empty(scraper, http):
    assert scraper.scrape_many([], http) == []


def test_batch_concurrency_must_be_positive(scraper, http, server):
    with pytest.raises(ValueError):
        scraper.scrape_many([f"{server}/article"], http, concurrency=0)

"""异步接口和按完成顺序的批量接口（同步 scrape_iter、异步 ascrape_many）。"""

import asyncio
import time

import pytest

import pagesift


def run(coro):
    return asyncio.run(coro)


def test_ascrape(scraper, http, server):
    d = run(scraper.ascrape(f"{server}/article", http))
    assert d.success and d.metadata["title"] == "Page /article"


def test_ascrape_raises(scraper, http, server):
    with pytest.raises(pagesift.RobotsDisallowed):
        run(scraper.ascrape(f"{server}/private/1", http))


def test_ascrape_gather_runs_concurrently(scraper, http, server, slow_probe):
    async def main():
        t = time.perf_counter()
        docs = await asyncio.gather(
            *(scraper.ascrape(f"{server}/slow?ms=400&i={i}&probe={slow_probe.id}", http) for i in range(8))
        )
        return docs, time.perf_counter() - t

    docs, elapsed = run(main())
    assert all(d.success for d in docs)
    assert slow_probe.peak >= 2, (slow_probe.peak, elapsed)


URLS_MIXED = ["/slow?ms=900", "/slow?ms=10", "/private/z"]


def check_completion_order(docs, urls):
    assert len(docs) == 3
    assert docs[-1].url == urls[0] and docs[-1].index == 0
    assert sorted(d.index for d in docs) == [0, 1, 2]
    assert [d.url for d in sorted(docs, key=lambda d: d.index)] == urls
    bad = next(d for d in docs if not d.success)
    assert isinstance(bad.error, pagesift.RobotsDisallowed) and bad.index == 2


def test_ascrape_many_yields_in_completion_order(scraper, http, server):
    urls = [server + u for u in URLS_MIXED]

    async def main():
        batch = scraper.ascrape_many(urls, http, concurrency=3)
        assert len(batch) == 3
        return [d async for d in batch]

    check_completion_order(run(main()), urls)


def test_scrape_iter_yields_in_completion_order(scraper, http, server):
    urls = [server + u for u in URLS_MIXED]
    check_completion_order(list(scraper.scrape_iter(urls, http, concurrency=3)), urls)


def test_batch_is_single_pass(scraper, http, server):
    batch = scraper.scrape_iter([f"{server}/page/1"], http)
    assert len(list(batch)) == 1
    assert list(batch) == []


def test_ascrape_many_break_and_cancel(scraper, http, server):
    # 只计“取消到结束”：第一条结果什么时候到由下面的测试管
    urls = [f"{server}/page/0"] + [f"{server}/slow?ms=30000&i={i}" for i in range(5)]

    async def main():
        batch = scraper.ascrape_many(urls, http, concurrency=6)
        async for d in batch:
            assert d.success
            break
        t = time.perf_counter()
        batch.cancel()
        rest = [d async for d in batch]  # 取消后迭代很快结束
        return time.perf_counter() - t, rest

    elapsed, rest = run(main())
    assert elapsed < 1 and rest == []


def test_fast_page_not_blocked_by_slow_pages(scraper, http, server):
    # 慢页面排在前面先占住同一主机的连接名额；快页面的结果时间不能取决于慢页面多慢
    urls = [f"{server}/slow?ms=30000&i={i}" for i in range(5)] + [f"{server}/page/fast"]

    async def main():
        t = time.perf_counter()
        batch = scraper.ascrape_many(urls, http, concurrency=6)
        async for d in batch:
            first = (time.perf_counter() - t, d.url)
            break
        batch.cancel()
        return first

    elapsed, url = run(main())
    assert url.endswith("/page/fast") and elapsed < 5, (elapsed, url)


def test_scrape_iter_cancel(scraper, http, server):
    urls = [f"{server}/slow?ms=5000&i={i}" for i in range(4)]
    t = time.perf_counter()
    batch = scraper.scrape_iter(urls, http)
    batch.cancel()
    assert list(batch) == []
    assert time.perf_counter() - t < 2


def test_module_level_async(server):
    async def main():
        with pytest.raises(pagesift.SsrfBlocked):
            await pagesift.ascrape(f"{server}/article", engine="http")
        return [d async for d in pagesift.ascrape_many([f"{server}/a", "ftp://x/"], engine="http")]

    docs = run(main())
    assert sorted(d.error_code for d in docs) == ["INVALID_REQUEST", "SSRF_BLOCKED"]


def test_module_level_iter(server):
    docs = list(pagesift.scrape_iter([f"{server}/a"], engine="http"))
    assert docs[0].error_code == "SSRF_BLOCKED"
    assert pagesift.default_scraper() is pagesift.default_scraper()


def test_async_context_manager_closes(server):
    async def main():
        s = pagesift.Scraper(allow_private_network=True, request_interval=0)
        async with s:
            d = await s.ascrape(f"{server}/article", engine="http")
        await s.aclose()  # 重复关闭没问题
        return d

    assert run(main()).success

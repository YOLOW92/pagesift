"""整站爬取（crawl）和站点链接列表（map）。"""

import asyncio

import pytest

import pagesift


def test_crawl_yields_pages_and_errors(scraper, http, server):
    crawl = scraper.crawl(f"{server}/site/", http, sitemap="skip", limit=10)
    assert isinstance(crawl, pagesift.Crawl)
    assert crawl.summary is None
    docs = list(crawl)
    ok = {d.url: d for d in docs if d.success}
    bad = [d for d in docs if not d.success]
    assert sorted(ok) == [f"{server}/site/", f"{server}/site/a", f"{server}/site/b"]
    assert ok[f"{server}/site/a"].metadata["title"] == "Site A"
    assert all(d.index is None for d in docs)
    assert [d.url for d in bad] == [f"{server}/site/secret"]
    assert isinstance(bad[0].error, pagesift.RobotsDisallowed)
    assert (crawl.pages, crawl.errors) == (3, 1)
    assert crawl.summary["stop_reason"] == "exhausted"
    assert crawl.summary["pages"] == 3


def test_crawl_limit_and_async(scraper, http, server):
    async def main():
        crawl = scraper.crawl(f"{server}/site/", http, sitemap="skip", limit=2)
        docs = [d async for d in crawl]
        return docs, crawl.summary

    docs, summary = asyncio.run(main())
    assert len(docs) == 2, [(d.url, d.success, d.error_code) for d in docs]
    assert summary["stop_reason"] == "limit_reached"


def test_crawl_cancel_ends_iteration(scraper, http, server):
    crawl = scraper.crawl(f"{server}/site/", http, sitemap="skip", max_concurrency=1)
    first = next(crawl)
    assert first.url == f"{server}/site/"
    crawl.cancel()
    assert list(crawl) == []


def test_crawl_validation(scraper, http, server):
    with pytest.raises(pagesift.InvalidRequest):
        scraper.crawl("ftp://example.com/", http)
    with pytest.raises(pagesift.InvalidRequest):
        scraper.crawl(f"{server}/site/", http, sitemap="never")
    with pytest.raises(pagesift.InvalidRequest):
        scraper.crawl(f"{server}/site/", http, adaptive=True)
    # 正则写错由引擎发现：迭代时抛出
    with pytest.raises(pagesift.InvalidRequest):
        list(scraper.crawl(f"{server}/site/", http, include_paths=["(["]))


def test_map(scraper, server):
    links = scraper.map(f"{server}/site/", sitemap="skip")
    urls = [link["url"] for link in links]
    assert f"{server}/site/a" in urls and f"{server}/site/b" in urls
    assert set(links[0]) == {"url", "title", "description"}
    assert asyncio.run(scraper.amap(f"{server}/site/", sitemap="skip")) == links
    with pytest.raises(pagesift.InvalidRequest):
        scraper.map("not a url")

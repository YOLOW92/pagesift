"""同步接口、Document 字段、选项校验。"""

import pickle

import pytest

import pagesift


def test_scrape_fields(scraper, http, server):
    d = scraper.scrape(f"{server}/article", http)
    assert d.success and d.error is None and d.error_message is None
    assert d.url == f"{server}/article"
    assert d.final_url == f"{server}/article"
    assert d.status_code == 200
    assert d.markdown.startswith("# Page /article")
    assert "Copyright footer" not in d.markdown
    assert d.metadata["title"] == "Page /article"
    assert d.metadata["description"] == "About Page /article"
    assert d.metadata["og_image"] == "/og.png"
    assert d.engine == "http"
    # 正文哈希总有；原始 HTML 和纯文本的哈希只在请求变化检测时计算
    assert len(d.content_hash) == 64 and d.raw_hash is None
    assert d.elapsed_ms > 0 and {"fetch", "extract", "total"} <= set(d.timings)
    # 默认只有 markdown
    assert d.html is None and d.raw_html is None and d.links is None and d.images is None


def test_formats_and_redirect(scraper, http, server):
    d = scraper.scrape(
        f"{server}/redirect", http, formats=["markdown", "html", "raw_html", "links", "images"]
    )
    assert d.url == f"{server}/redirect"
    assert d.final_url == f"{server}/article"
    assert d.redirect_chain == [f"{server}/redirect"]
    assert d.html.startswith("<article>")
    assert d.raw_html.startswith("<!DOCTYPE html>")
    assert f"{server}/next" in d.links
    assert d.images == [f"{server}/pic.png"]


def test_same_content_same_fingerprint(scraper, http, server):
    a = scraper.scrape(f"{server}/article", http)
    b = scraper.scrape(f"{server}/redirect", http)
    assert a.content_hash == b.content_hash


def test_only_main_content(scraper, http, server):
    full = scraper.scrape(f"{server}/article", http, only_main_content=False)
    assert "Copyright footer" in full.markdown


def test_headers(scraper, http, server):
    d = scraper.scrape(f"{server}/headers", http, headers={"X-Test": "hello-pagesift"})
    assert "hello-pagesift" in d.markdown


def test_server_error_is_reported_not_raised(scraper, http, server):
    d = scraper.scrape(f"{server}/error500", http)
    assert d.success and d.status_code == 500


def test_to_dict(scraper, http, server):
    d = scraper.scrape(f"{server}/article", http).to_dict()
    assert d["success"] is True and d["error"] is None and d["index"] is None
    assert d["status_code"] == 200 and d["engine"] == "http"
    assert d["metadata"]["title"] == "Page /article"
    for key in ("index", "url", "final_url", "markdown", "html", "raw_html", "links", "images",
                "content_hash", "elapsed_ms", "timings", "warnings"):
        assert key in d


def test_auto_engine_keeps_http_for_long_pages(scraper, server):
    d = scraper.scrape(f"{server}/article")
    assert d.engine == "http"


def test_options_object(scraper, server):
    opts = pagesift.ScrapeOptions(engine="http", formats=["markdown", "links"])
    d = scraper.scrape(f"{server}/article", opts)
    assert d.links
    # 关键字参数覆盖 options 里的字段
    d = scraper.scrape(f"{server}/article", opts, formats=["markdown"])
    assert d.links is None
    assert opts.replace(mobile=True).mobile and not opts.mobile
    assert pagesift.ScrapeOptions(formats="links").formats == ("links",)


@pytest.mark.parametrize(
    "options, exc",
    [
        ({"timeout": 10}, pagesift.InvalidRequest),
        ({"timeout": 2000, "wait_for": 1500}, pagesift.InvalidRequest),
        ({"wait_for": 70_000}, pagesift.InvalidRequest),
        ({"engine": "stealth"}, pagesift.InvalidRequest),
        ({"formats": ["screenshot"]}, pagesift.UnsupportedOption),
        ({"include_tags": ["p["]}, pagesift.InvalidRequest),
        ({"exclude_tags": ["nav", ":hover"]}, pagesift.InvalidRequest),
    ],
)
def test_invalid_options_raise_early(options, exc):
    with pytest.raises(exc) as e:
        pagesift.ScrapeOptions(**options)
    assert isinstance(e.value, ValueError)
    assert e.value.code == exc.code


def test_unknown_option_is_a_type_error(scraper, server):
    with pytest.raises(TypeError):
        scraper.scrape(f"{server}/article", stealth=True)
    with pytest.raises(TypeError):
        scraper.scrape(f"{server}/article", {"engine": "http"})


def test_invalid_url(scraper):
    with pytest.raises(pagesift.InvalidRequest):
        scraper.scrape("ftp://example.com/file")
    with pytest.raises(pagesift.InvalidRequest):
        scraper.scrape("not a url")


def test_module_level_default_blocks_private_network(server):
    # 默认的 Scraper 不允许访问本机地址
    with pytest.raises(pagesift.SsrfBlocked):
        pagesift.scrape(f"{server}/article", engine="http")


def test_scraper_constructor_validation():
    with pytest.raises(ValueError):
        pagesift.Scraper(request_interval=-1)
    with pytest.raises(ValueError):
        pagesift.Scraper(per_host_concurrency=0)
    with pytest.raises(ValueError):
        pagesift.Scraper(browser_executable="/x", browser_url="ws://y")
    with pytest.raises(pagesift.InvalidRequest):
        pagesift.Scraper(user_agent="Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/120.0")


def test_convert_unchanged():
    html = """<html><head><title>T</title><meta name="author" content="A"></head>
    <body><nav>menu</nav><article><p>Hello <a href="/x">world</a></p></article></body></html>"""
    d = pagesift.convert(html, url="https://example.com/post")
    assert d.markdown.strip() == "Hello [world](https://example.com/x)"
    assert d.metadata["title"] == "T" and d.metadata["author"] == "A"
    assert "menu" in pagesift.convert(html, only_main_content=False).markdown
    with pytest.raises(ValueError):
        pagesift.convert(html, url="::bad::")


def test_to_dict_is_picklable(scraper, http, server):
    d = scraper.scrape(f"{server}/article", http)
    assert pickle.loads(pickle.dumps(d.to_dict()))["markdown"] == d.markdown


def test_fields_convert_lazily_and_cache(scraper, server):
    d = scraper.scrape(f"{server}/article", engine="http", formats=["markdown", "raw_html"])
    # 字符串字段第一次访问时转换，之后返回同一个对象
    assert d.markdown is d.markdown
    assert d.raw_html is d.raw_html
    assert d.index is None


def test_urls_must_not_be_a_string(scraper):
    with pytest.raises(TypeError):
        scraper.scrape_many("https://example.com")

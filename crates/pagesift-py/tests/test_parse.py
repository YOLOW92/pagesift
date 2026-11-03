"""解析本地文件（parse、aparse、parse_many）。"""

import asyncio

import pytest

import pagesift

HTML = (
    "<!DOCTYPE html><html><head><title>Parsed</title></head><body><article><h1>Parsed</h1>"
    + "".join(f"<p>Paragraph {i} of a normal article, with enough words to read like prose.</p>" for i in range(20))
    + '<p>See <a href="/next">the next page</a>.</p></article></body></html>'
).encode()


def test_parse_html(scraper):
    d = scraper.parse(HTML, "page.html")
    assert d.success and d.url == "page.html"
    assert d.markdown.startswith("# Parsed")
    assert d.metadata["title"] == "Parsed"
    based = scraper.parse(
        bytearray(HTML), "x", content_type="text/html", url="https://example.com/post/", formats=["links"]
    )
    assert "https://example.com/next" in based.links
    assert asyncio.run(scraper.aparse(HTML, "page.html")).markdown == d.markdown


def test_parse_rejects(scraper):
    with pytest.raises(pagesift.UnsupportedOption):
        scraper.parse(b"just bytes", "blob.bin")
    with pytest.raises(pagesift.UnsupportedOption):
        scraper.parse(HTML, "page.html", engine="browser")
    with pytest.raises(TypeError):
        scraper.parse("text", "page.html")


def test_parse_many_in_completion_order(scraper):
    files = [(HTML, "a.html"), (b"???", "b.bin"), (HTML, "c.htm")]
    batch = scraper.parse_many(files, concurrency=2)
    assert len(batch) == 3
    docs = sorted(batch, key=lambda d: d.index)
    assert [d.url for d in docs] == ["a.html", "b.bin", "c.htm"]
    assert docs[0].success and docs[2].success
    assert isinstance(docs[1].error, pagesift.UnsupportedOption)

    async def main():
        return [d async for d in scraper.parse_many(files)]

    assert len(asyncio.run(main())) == 3

"""GIL：抓取和转换期间释放 GIL，Python 线程能真正并行；批量接口的并发在 Rust 侧完成。"""

import threading
import time
from concurrent.futures import ThreadPoolExecutor

import pagesift
from conftest import PROSE


def test_threads_scrape_in_parallel(scraper, http, server, slow_probe):
    urls = [f"{server}/slow?ms=400&i={i}&probe={slow_probe.id}" for i in range(8)]

    t = time.perf_counter()
    for u in urls[:3]:
        assert scraper.scrape(u, http).success
    serial_per_page = (time.perf_counter() - t) / 3

    t = time.perf_counter()
    with ThreadPoolExecutor(8) as pool:
        docs = list(pool.map(lambda u: scraper.scrape(u, http), urls))
    threaded = time.perf_counter() - t

    assert all(d.success for d in docs)
    # 8 个页面串行约 3.2 秒；释放 GIL 后线程并行，接近单页耗时
    assert slow_probe.peak >= 2, (slow_probe.peak, threaded, serial_per_page)


def test_scrape_many_uses_rust_concurrency(scraper, http, server, slow_probe):
    urls = [f"{server}/slow?ms=400&i={i}&probe={slow_probe.id}" for i in range(8)]
    t = time.perf_counter()
    docs = scraper.scrape_many(urls, http, concurrency=8)
    elapsed = time.perf_counter() - t
    assert all(d.success for d in docs)
    assert slow_probe.peak >= 2, (slow_probe.peak, elapsed)


def test_python_threads_run_during_convert():
    html = "<html><body><article>" + PROSE * 400 + "</article></body></html>"
    done = threading.Event()
    result = {}

    def work():
        t = time.perf_counter()
        result["doc"] = pagesift.convert(html)
        result["elapsed"] = time.perf_counter() - t
        done.set()

    th = threading.Thread(target=work)
    ticks = 0
    th.start()
    while not done.is_set():
        ticks += 1
    th.join()
    assert result["doc"].markdown
    # 持有 GIL 的话主线程几乎不能运行；释放后主线程在转换期间一直在跑
    assert result["elapsed"] > 0.01 and ticks > 10_000, (ticks, result["elapsed"])


def test_convert_concurrent_results(record_property):
    html = "<html><body><article>" + PROSE * 100 + "</article></body></html>"
    n = 8
    t = time.perf_counter()
    expected = [pagesift.convert(html).markdown for _ in range(n)]
    serial = time.perf_counter() - t
    t = time.perf_counter()
    with ThreadPoolExecutor(4) as pool:
        actual = list(pool.map(pagesift.convert, [html] * n))
    threaded = time.perf_counter() - t
    assert [doc.markdown for doc in actual] == expected
    # Shared runners have no guaranteed four-core CPU budget. Record throughput;
    # the separate busy-loop test directly verifies that conversion releases the GIL.
    record_property("serial_seconds", serial)
    record_property("threaded_seconds", threaded)

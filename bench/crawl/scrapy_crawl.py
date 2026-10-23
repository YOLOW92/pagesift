"""Scrapy 整站爬取：每个站点从首页出发，跟随站内链接，每站最多 pages 页。

<venv>/bin/python -I scrapy_crawl.py <port> <hosts> <pages> <interval-ms> [per-host]
礼貌规则对齐 pagesift：遵守 robots.txt，每主机最小间隔 interval，每主机并发 per-host。
Scrapy 只抓取和解析链接，不做正文提取（这是它和其他工具的差别，结果里注明）。
"""
import sys
from collections import Counter
from urllib.parse import urlsplit

import scrapy
from scrapy.crawler import CrawlerProcess
from scrapy.linkextractors import LinkExtractor


def main():
    port, hosts, pages, interval = (int(x) for x in sys.argv[1:5])
    per_host = int(sys.argv[5]) if len(sys.argv) > 5 else 2
    start_host = int(sys.argv[6]) if len(sys.argv) > 6 else 0
    domains = [f"s{i}.localhost" for i in range(start_host, start_host + hosts)]
    counts = Counter()
    # allow_domains 会连端口一起比较，对不上 s{i}.localhost:port，所以在 parse 里按主机名过滤
    links = LinkExtractor()
    allowed = set(domains)

    class Site(scrapy.Spider):
        name = "site"
        allowed_domains = domains
        start_urls = [f"http://{d}:{port}/" for d in domains]

        def parse(self, response):
            host = urlsplit(response.url).hostname
            counts[host] += 1
            for link in links.extract_links(response):
                h = urlsplit(link.url).hostname
                if h in allowed and counts[h] < pages:
                    yield response.follow(link.url, self.parse)

    process = CrawlerProcess(settings={
        "ROBOTSTXT_OBEY": True,
        "USER_AGENT": "pagesift-bench-scrapy/1.0 (+https://github.com/YOLOW92/pagesift)",
        "CONCURRENT_REQUESTS": max(hosts * per_host, 16),
        "CONCURRENT_REQUESTS_PER_DOMAIN": per_host,
        "DOWNLOAD_DELAY": interval / 1000,
        "RANDOMIZE_DOWNLOAD_DELAY": False,
        "AUTOTHROTTLE_ENABLED": False,
        "LOG_LEVEL": "ERROR",
        "TELNETCONSOLE_ENABLED": False,
        # 每站 pages 页的上限由 parse 控制；这里只防止意外的无限爬
        "CLOSESPIDER_PAGECOUNT": hosts * pages + hosts,
        "REACTOR_THREADPOOL_MAXSIZE": 20,
        # 源站在 ::1 上，默认的解析器只给 IPv4
        "DNS_RESOLVER": "scrapy.resolver.CachingHostnameResolver",
    })
    process.crawl(Site)
    process.start()
    print(f"scrapy pages={sum(counts.values())}", file=sys.stderr)


if __name__ == "__main__":
    main()

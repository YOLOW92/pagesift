# Crawl frontier results, 2026-10-21

Machine: Intel Core i7-10700, 15 GB RAM, Ubuntu (Linux 7.0). pagesift 0.12.0 (commits afb6f89, ea8da7c) versus the crawler before them (20:20 build of 7485617..667b4b5).
Competitors: Crawl4AI 0.9.5 (HTTP strategy), crw 0.38.0 (AGPL, evaluation only), Spider 2.53, Scrapy 2.19.

## Simulated web (`run_wild.sh`)

40 sites × 60 content pages, crawl limit 60 per site, base latency 20 ms. Coverage ↑, waste ↓, robots violations ↓, 429s ↓, time ↓, memory ↓ (peak PSS of the process tree).

### Seed 1

In this table the `pagesift` rows are the crawler **before** the changes (baseline); the run after the changes follows below.

```
场景       工具                 覆盖率↑     浪费率↓     请求数   robots违规↓   429↓     耗时s↓
fast     crawl4ai-http     0.492    0.446    2473           0      6   125.31
fast     crw               0.324    0.371    1971           0    339    15.51
fast     pagesift          0.260    0.449    1404           0     48     1.34
fast     scrapy            0.319    0.435    3545           0   1049    17.24
fast     spider            0.314    0.373    1961           0    335     1.41
polite   crawl4ai-http     0.505    0.422    2469           0      0   141.52
polite   crw               0.324    0.371    1971           0    339     15.4
polite   pagesift          0.283    0.487    1482           0      0     12.0
polite   scrapy            0.185    0.727    2622           0     40    15.41
polite   spider            0.455    0.447    2342           0      0    12.51

fast crawl4ai-http {"429": 6, "503": 133, "binary": 190, "content": 1182, "dup": 677, "nav": 50, "trap": 235}
fast crw {"429": 339, "503": 77, "content": 778, "dup": 623, "nav": 46, "trap": 108}
fast pagesift {"429": 48, "503": 59, "binary": 104, "content": 625, "dup": 435, "nav": 42, "trap": 91}
fast scrapy {"429": 1049, "503": 118, "content": 766, "dup": 883, "nav": 69, "trap": 660}
fast spider {"404": 1, "429": 335, "503": 78, "content": 754, "dup": 605, "nav": 62, "trap": 126}
polite crawl4ai-http {"503": 134, "binary": 185, "content": 1211, "dup": 667, "nav": 82, "trap": 190}
polite crw {"429": 339, "503": 77, "content": 778, "dup": 623, "nav": 46, "trap": 108}
polite pagesift {"503": 19, "binary": 105, "content": 680, "dup": 514, "nav": 61, "trap": 103}
polite scrapy {"429": 40, "503": 146, "content": 444, "dup": 594, "nav": 87, "trap": 1311}
polite spider {"404": 1, "503": 121, "content": 1093, "dup": 876, "nav": 81, "trap": 170}
```

### Seed 2

All rows after the changes.

```
场景       工具                 覆盖率↑     浪费率↓     请求数   robots违规↓   429↓     耗时s↓
fast     crawl4ai-http     0.499    0.429    2369         102      6   113.98
fast     crw               0.367    0.403    2086           0    236     14.9
fast     pagesift          0.747    0.116    2316           0     81    21.42
fast     scrapy            0.332    0.555    2840           0    297    18.44
fast     spider            0.358    0.367    2003           0    271     1.35
polite   crawl4ai-http     0.529    0.392    2365          60      0    113.5
polite   crw               0.367    0.403    2086           0    236    14.99
polite   pagesift          0.736    0.120    2200           0      0    42.28
polite   scrapy            0.197    0.725    2588           0     48    14.57
polite   spider            0.470    0.436    2282           0      0    12.46

fast crawl4ai-http {"429": 6, "503": 99, "binary": 150, "content": 1197, "dup": 619, "nav": 51, "robots": 102, "trap": 145}
fast crw {"429": 236, "503": 78, "content": 880, "dup": 730, "nav": 52, "trap": 110}
fast pagesift {"429": 81, "503": 91, "content": 1793, "dup": 181, "nav": 82, "trap": 88}
fast scrapy {"429": 297, "503": 102, "content": 797, "dup": 980, "nav": 67, "trap": 597}
fast spider {"429": 271, "503": 69, "content": 860, "dup": 626, "nav": 67, "trap": 110}
polite crawl4ai-http {"503": 87, "binary": 127, "content": 1269, "dup": 605, "nav": 83, "robots": 60, "trap": 134}
polite crw {"429": 236, "503": 78, "content": 880, "dup": 730, "nav": 52, "trap": 110}
polite pagesift {"503": 89, "content": 1767, "dup": 175, "nav": 81, "trap": 88}
polite scrapy {"429": 48, "503": 103, "content": 472, "dup": 809, "nav": 88, "trap": 1068}
polite spider {"503": 82, "content": 1128, "dup": 840, "nav": 78, "trap": 154}
```

Seed 1, pagesift after the changes:

```
场景       工具                 覆盖率↑     浪费率↓     请求数   robots违规↓   429↓     耗时s↓
fast     pagesift          0.762    0.120    2442           0    120    20.48

fast pagesift {"429": 120, "503": 117, "content": 1830, "dup": 177, "nav": 83, "trap": 115}
```

## Real sites (`real_sites.py`)

40 random Tranco sites (seed 20261021), limit 40 pages, HTTP engine, robots.txt respected, default rate limits. Old binary always ran first in this run (the alternating order in the script was added afterwards).

| Site | Old pages | Old distinct | Old errors | New pages | New distinct | New errors |
|---|---|---|---|---|---|---|
| https://www.iseeme.com:443/en-us/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://lotterysambadresult.in/ | 27 | 24 | 13 | 27 | 24 | 13 |
| https://www.eno.org/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://coastalabc.com/ | 40 | 34 | 0 | 40 | 34 | 0 |
| https://www.repertuarim.com/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://knoxt.space/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://www.agriculture.com/ | 39 | 39 | 1 | 36 | 36 | 4 |
| https://xivmodarchive.com/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://www.chatbeacon.ai/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://www.elconfidencialdigital.com/ | 34 | 34 | 6 | 29 | 29 | 11 |
| https://vizortube.com/ | 10 | 9 | 4 | 10 | 9 | 4 |
| https://www.cmt.com/ | 40 | 3 | 0 | 40 | 3 | 0 |
| https://chl.la/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://www.adminit.cz/ | 40 | 39 | 0 | 40 | 39 | 0 |
| https://www.teamwork.com/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://wiocha.pl/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://dashnet.org/ | 3 | 2 | 0 | 3 | 2 | 0 |
| https://www.mnb.hu/web/fooldal | 40 | 40 | 0 | 40 | 40 | 0 |
| https://www.naughtymachinima.com/ | 0 | 0 | 1 | 40 | 40 | 0 |
| https://www.cheapjerseyfan.com/ | 40 | 37 | 0 | 40 | 37 | 0 |
| https://www.luxuryrealestate.com/ | 40 | 37 | 0 | 40 | 37 | 0 |
| https://asiaplus.news/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://riftdecks.com/ | 40 | 39 | 0 | 40 | 39 | 0 |
| https://www.kbs.de/DE/Home/node.html | 40 | 38 | 0 | 40 | 38 | 0 |
| https://www.spectator.com.au/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://m32connect.com/ | 40 | 40 | 0 | 40 | 39 | 0 |
| https://yutex.ru/index.html | 39 | 17 | 1 | 40 | 17 | 0 |
| https://wheeloffortune.com/ | 40 | 39 | 0 | 40 | 39 | 0 |
| https://gcras.ru/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://www.19fortyfive.com/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://jogabets.co.mz/ | 40 | 39 | 0 | 40 | 39 | 0 |
| https://www.citi.com/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://us.babor.com/ | 40 | 13 | 0 | 40 | 13 | 0 |
| https://bandsworksconcerts.info/ | 39 | 37 | 1 | 40 | 38 | 0 |
| https://tiengdong.com/ | 37 | 37 | 3 | 40 | 40 | 0 |
| https://newfs.info/ | 39 | 39 | 1 | 39 | 39 | 1 |
| https://www.iseecars.com/ | 40 | 39 | 0 | 40 | 39 | 0 |
| https://opticleanpro.com/ | 2 | 2 | 0 | 2 | 2 | 0 |
| https://www.livetennis.it/ | 40 | 40 | 0 | 40 | 40 | 0 |
| https://www.playerup.com/ | 0 | 0 | 1 | 0 | 0 | 1 |
| **Total** | 1389 | 1277 | 32 | 1426 | 1312 | 34 |

More pages and distinct pages are better; fewer errors are better. The extra errors on agriculture.com and elconfidencialdigital.com are Cloudflare challenges returned to the second run.

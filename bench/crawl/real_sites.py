"""真实网站随机回归测试：两个 pagesift 二进制（改动前、改动后）各爬同一批随机真实站点，
比较交付页数、独立内容页数（按正文去重）和错误数。用来确认爬取规则在真实网站上没有误伤。

python3 -I real_sites.py <旧二进制> <新二进制> <站点清单 manifest.jsonl> <种子> <站点数> <每站上限>

站点清单取 bench/fresh 的随机语料（Tranco 随机站点的首页，kind=home 的行）。按种子随机抽站；
站点之间 8 个并行，同一站点两个版本依次跑，奇数站点先跑新版、偶数站点先跑旧版（后跑的那个
更容易碰上站点的访问拦截，交替顺序抵消这个偏差）。每次爬取 HTTP 引擎、整站范围、遵守 robots.txt，
限速用 pagesift 默认值。每行输出一个站点的 JSON，最后一行是合计。
"""
import concurrent.futures as cf, hashlib, json, random, re, subprocess, sys

old_bin, new_bin, manifest = sys.argv[1:4]
seed, n_sites, limit = (int(x) for x in sys.argv[4:7])
homes = sorted({json.loads(l)['final_url'] for l in open(manifest) if json.loads(l).get('kind') == 'home'})
random.Random(seed).shuffle(homes)
sites = homes[:n_sites]


def norm(md):
    return hashlib.sha1(re.sub(r'[^0-9A-Za-z\u0080-￿]+', '', md or '').encode()).hexdigest()


def crawl(binary, url):
    try:
        p = subprocess.run([binary, 'crawl', url, '--limit', str(limit), '--engine', 'http', '--entire-domain'],
                           capture_output=True, text=True, timeout=600)
        out, err = p.stdout, p.stderr
    except subprocess.TimeoutExpired as e:
        out = e.stdout.decode() if isinstance(e.stdout, bytes) else (e.stdout or '')
        err = ''
    pages, texts, errors = 0, set(), 0
    for line in out.splitlines():
        if '{' not in line:
            continue
        try:
            r = json.loads(line[line.index('{'):])
        except json.JSONDecodeError:
            continue
        pages += 1
        texts.add(norm(r.get('markdown')))
    m = re.search(r'(\d+) pages, (\d+) errors', out + err)
    if m:
        errors = int(m.group(2))
    return {'pages': pages, 'unique': len(texts), 'errors': errors}


def one(i_url):
    i, url = i_url
    if i % 2:
        new = crawl(new_bin, url)
        old = crawl(old_bin, url)
    else:
        old = crawl(old_bin, url)
        new = crawl(new_bin, url)
    return url, old, new


rows = []
with cf.ThreadPoolExecutor(8) as ex:
    for url, old, new in ex.map(one, enumerate(sites)):
        rows.append((url, old, new))
        print(json.dumps({'url': url, 'old': old, 'new': new}), flush=True)
total = {v: {k: sum(r[1 if v == 'old' else 2][k] for r in rows) for k in ['pages', 'unique', 'errors']}
         for v in ['old', 'new']}
print(json.dumps({'total': total}))

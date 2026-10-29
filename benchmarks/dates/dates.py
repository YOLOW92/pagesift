"""发布日期提取对比：htmldate 自带的人工标注评测集（eval_default 800 页 + eval_mediacloud_2020 200 页）。

<py-venv>/bin/python -I benchmarks/dates/dates.py [--url] [--tools a,b,...]

口径和 htmldate 的 tests/comparison.py 相同：按天（YYYY-MM-DD）比较；输出空时，标注也为空算 TN，否则算 FN；
输出了日期但不对算 FP。精确率、召回率、准确率、F1 都是越高越好；耗时越低越好。
默认不把 URL 交给工具（官方口径）；--url 时把页面 URL 也交给支持的工具（真实使用时我们总是知道 URL）。
按 sha1(url) % 2 把页面分成调参集（tune）和保留集（hold），分别报告：只在保留集上的改进才算数。
数据：benchmarks/data/fresh/data/htmldate（git clone adbar/htmldate，Apache-2.0）。
"""
import argparse, hashlib, json, os, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DATA = os.path.join(ROOT, 'benchmarks/data/fresh/data/htmldate/tests')
PAGESIFT = os.environ.get('PAGESIFT_META_EVAL', os.path.join(ROOT, 'target/release/examples/meta_eval'))


def pages():
    out = []
    for name in ['eval_mediacloud_2020.json', 'eval_default.json']:
        for url, d in json.load(open(os.path.join(DATA, name), encoding='utf-8')).items():
            for sub in ('test_set', 'cache', 'eval'):
                p = os.path.join(DATA, sub, d['file'])
                if os.path.isfile(p):
                    break
            else:
                continue
            half = 'hold' if int(hashlib.sha1(url.encode()).hexdigest(), 16) % 2 else 'tune'
            out.append({'url': url, 'path': p, 'gold': d['date'], 'set': name.split('_')[1].split('.')[0], 'half': half})
    return out


def run_pagesift(ps, use_url):
    with tempfile.NamedTemporaryFile('w', suffix='.txt', delete=False) as f:
        for p in ps:
            f.write(p['path'] + ('\t' + p['url'] if use_url else '') + '\n')
        lst = f.name
    out = subprocess.run([PAGESIFT, lst], capture_output=True, text=True, check=True).stdout
    os.unlink(lst)
    res = {}
    for line in out.splitlines():
        r = json.loads(line)
        d = r.get('published_local') or r.get('published')
        res[r['file']] = ((d or '')[:10] or None, r['ms'] / 1000)
    return res


def python_tools(use_url):
    from htmldate import find_date
    from htmldate.utils import decode_file
    tools = {}
    tools['htmldate (fast)'] = lambda h, u: find_date(h, url=u if use_url else None, original_date=True, extensive_search=False)
    tools['htmldate (extensive)'] = lambda h, u: find_date(h, url=u if use_url else None, original_date=True, extensive_search=True)
    try:
        from trafilatura import extract_metadata
        def traf(h, u):
            m = extract_metadata(h, default_url=u if use_url else None)
            return m.date if m else None
        tools['trafilatura'] = traf
    except ImportError:
        pass
    try:
        from newspaper import Article, parsers
        def news(h, u):
            try:
                a = Article(url=u if use_url else '')
                a.download(input_html=h)
                a.doc = parsers.fromstring(a.html)
                if a.doc is None:
                    return None
                d = a.extractor.get_publishing_date(a.url, a.doc)
            except Exception:
                return None
            return str(d)[:10] if d else None
        tools['newspaper4k'] = news
    except ImportError:
        pass
    try:
        from goose3 import Goose
        g = Goose()
        def goose(h, u):
            try:
                a = g.extract(raw_html=h)
            except Exception:
                return None
            d = a.publish_datetime_utc or a.publish_date
            return str(d)[:10] if d else None
        tools['goose3'] = goose
    except ImportError:
        pass
    return tools, decode_file


def score(rows):
    tp = fp = tn = fn = 0
    for gold, got in rows:
        if got is None:
            if gold is None:
                tn += 1
            fn += 1
        elif got == gold:
            tp += 1
        else:
            fp += 1
    prec = tp / (tp + fp) if tp + fp else 0
    rec = tp / (tp + fn) if tp + fn else 0
    acc = (tp + tn) / (tp + tn + fp + fn)
    f1 = 2 * tp / (2 * tp + fp + fn)
    return prec, rec, acc, f1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--url', action='store_true')
    ap.add_argument('--tools', default='')
    ap.add_argument('--dump', default='', help='每页每个工具的结果写到这个 JSON 文件')
    a = ap.parse_args()
    ps = pages()
    want = set(filter(None, a.tools.split(',')))
    results = {}
    if not want or 'pagesift' in want:
        r = run_pagesift(ps, a.url)
        results['pagesift'] = {p['path']: r[p['path']] for p in ps}
    tools, decode = python_tools(a.url)
    for name, fn in tools.items():
        if want and name.split(' ')[0] not in want and name not in want:
            continue
        res = {}
        for p in ps:
            html = decode(open(p['path'], 'rb').read())
            t = time.perf_counter()
            try:
                got = fn(html, p['url'])
            except Exception:
                got = None
            res[p['path']] = (got, time.perf_counter() - t)
        results[name] = res
    print(f"{len(ps)} pages ({sum(p['half'] == 'hold' for p in ps)} hold); URL given: {a.url}")
    print(f"{'tool':22} {'half':5} {'prec↑':>6} {'rec↑':>6} {'acc↑':>6} {'F1↑':>6} {'ms/page↓':>9}")
    for name, res in results.items():
        for half in ('all', 'tune', 'hold'):
            sel = [p for p in ps if half == 'all' or p['half'] == half]
            pr, rc, ac, f1 = score([(p['gold'], res[p['path']][0]) for p in sel])
            ms = 1000 * sum(res[p['path']][1] for p in sel) / len(sel)
            print(f"{name:22} {half:5} {pr:6.3f} {rc:6.3f} {ac:6.3f} {f1:6.3f} {ms:9.2f}")
    if a.dump:
        json.dump({'pages': ps, 'results': {k: {p: v[0] for p, v in r.items()} for k, r in results.items()}},
                  open(a.dump, 'w'), ensure_ascii=False)


main()

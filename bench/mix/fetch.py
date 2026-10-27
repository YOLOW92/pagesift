"""混合增强基准的网络抓取（RULES.md 第三节）：A2 网页快照（存 WARC），B2 PDF 下载。只用标准库。

    python3 -I bench/mix/fetch.py <a2|b2> [--samples bench/mix/samples] [--out $MIX/inputs] [--threads 16]

- 输入 samples/<子类>.tsv（sample.py 的输出），每份只抓一次；抓不到的记“不可用”和原因，不换样本。
- 遵守 robots.txt（urllib.robotparser，按我们的 UA 判断；重定向后的地址也判断），UA 写仓库地址。
  401/403/需要登录的记“不可用”。同一主机两次请求至少隔 1 秒（arXiv 3 秒），不同主机并行。
- A2：内容存 <out>/a2/<id>.html，响应状态和头存 <id>.html.headers.json（replay.py 照着回放），全部响应（含内链所在的
  首页）另存 <out>/a2/snapshots.warc.gz（WARC/1.1，每条记录单独 gzip）。“inner” 样本先抓首页，再按种子从首页上的
  同站链接里挑一条抓。
- B2：PDF 存 <out>/b2/<id>.pdf；GOV.UK 先查内容 API 取页面上的第一个 PDF 附件。
- `fetch.py a2 --retry-failed`（2026-10-27 冻结后补的抓取规则，见 RULES 第七节）：只重抓上次失败里的两类——
  裸域名查不到地址（改抓 https://www.<域名>/）和暂时性错误（超时、DNS 暂时失败、连接被重置，隔 3 分钟重试，
  最多 2 次）；其余失败（robots、403、证书、首页没有同站链接等）照旧。结果就地更新 fetched.tsv，原因列写明怎么补的。
- 输出 samples/<子类>.fetched.tsv：id、状态、最终 URL、HTTP 状态、Content-Type、抓取时间（UTC）、字节数、SHA-256、
  本地路径（相对 --out）、html lang（A2）、原因。另写 <out>/<子类>.manifest 给 driver.py（id、路径、原始 URL）。
"""
import argparse
import gzip
import hashlib
import json
import random
import re
import threading
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import urllib.robotparser
import uuid
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from html.parser import HTMLParser
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONF = tomllib.loads((HERE / "seeds.toml").read_text())
UA = "pagesift-mix-bench/0.80 (+https://github.com/YOLOW92/pagesift)"
GAP = {"export.arxiv.org": 3.0, "arxiv.org": 3.0}
MAX_BYTES = {"a2": 50 * 2**20, "b2": 300 * 2**20}
HOP = {"connection", "keep-alive", "transfer-encoding", "content-encoding", "content-length", "proxy-connection", "upgrade"}


class Unavailable(Exception):
    pass


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """自己跟重定向，好在每一跳判断 robots.txt。"""

    def redirect_request(self, *a, **kw):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


class Fetcher:
    def __init__(self, warc=None):
        self.lock = threading.Lock()
        self.host_locks = {}
        self.last = {}
        self.robots = {}
        self.warc = warc

    def _host_lock(self, host):
        with self.lock:
            return self.host_locks.setdefault(host, threading.Lock())

    def _raw(self, url, max_bytes):
        host = urllib.parse.urlsplit(url).hostname or ""
        with self._host_lock(host):
            wait = self.last.get(host, 0) + GAP.get(host, 1.0) - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept-Encoding": "identity", "Accept": "*/*"})
            try:
                try:
                    r = OPENER.open(req, timeout=30)
                except urllib.error.HTTPError as e:
                    r = e  # 3xx（不自动跟）和 4xx/5xx 都有状态和头
                with r:
                    body = r.read(max_bytes + 1)
                    status, headers = r.status if hasattr(r, "status") else r.code, list(r.headers.items())
            finally:
                self.last[host] = time.monotonic()
        if len(body) > max_bytes:
            raise Unavailable(f"超过 {max_bytes >> 20} MB")
        return status, headers, body

    def allowed(self, url):
        p = urllib.parse.urlsplit(url)
        key = f"{p.scheme}://{p.netloc}"
        with self.lock:
            rp = self.robots.get(key)
        if rp is None:
            rp = urllib.robotparser.RobotFileParser()
            try:
                status, _, body = self._raw(key + "/robots.txt", 2**20)
            except Exception:  # noqa: BLE001  robots.txt 拿不到（网络错误）当作允许，与 robotparser 对 4xx 的处理一致
                status, body = 404, b""
            if status in (401, 403):
                rp.disallow_all = True
            elif status >= 500:
                rp.disallow_all = True  # RFC 9309：服务器错误时视为全部禁止
            elif 200 <= status < 300:
                rp.parse(body.decode("utf-8", "replace").splitlines())
            else:
                rp.allow_all = True
            with self.lock:
                self.robots[key] = rp
        return rp.can_fetch(UA, url)

    def get(self, url, max_bytes):
        """跟最多 10 次重定向，每一跳都判断 robots。返回 (最终 URL, 状态, 头, 内容)。"""
        for _ in range(10):
            if not self.allowed(url):
                raise Unavailable(f"robots.txt 禁止：{url}")
            status, headers, body = self._raw(url, max_bytes)
            self.record(url, status, headers, body)
            loc = next((v for k, v in headers if k.lower() == "location"), None)
            if status in (301, 302, 303, 307, 308) and loc:
                url = urllib.parse.urljoin(url, loc)
                continue
            if status in (401, 402, 403, 407):
                raise Unavailable(f"HTTP {status}（需要登录、付费或拒绝访问）")
            if status >= 400:
                raise Unavailable(f"HTTP {status}")
            return url, status, headers, body
        raise Unavailable("重定向超过 10 次")

    def record(self, url, status, headers, body):
        if not self.warc:
            return
        http = f"HTTP/1.1 {status}\r\n".encode() + b"".join(f"{k}: {v}\r\n".encode("latin-1", "replace") for k, v in headers) + b"\r\n" + body
        head = (f"WARC/1.1\r\nWARC-Type: response\r\nWARC-Record-ID: <urn:uuid:{uuid.uuid4()}>\r\n"
                f"WARC-Date: {datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}\r\nWARC-Target-URI: {url}\r\n"
                f"WARC-Payload-Digest: sha256:{hashlib.sha256(body).hexdigest()}\r\n"
                f"Content-Type: application/http;msgtype=response\r\nContent-Length: {len(http)}\r\n\r\n").encode()
        rec = gzip.compress(head + http + b"\r\n\r\n")
        with self.lock:
            self.warc.write(rec)


class Links(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.hrefs = []

    def handle_starttag(self, tag, attrs):
        if tag == "a":
            href = dict(attrs).get("href")
            if href:
                self.hrefs.append(href)


def inner_link(base, body, seed, id_):
    """首页上的同站 http(s) 链接（去掉锚点、明显的文件下载），排序去重后按种子挑一条。"""
    p = Links()
    try:
        p.feed(body.decode("utf-8", "replace"))
    except Exception:  # noqa: BLE001
        pass
    host = urllib.parse.urlsplit(base).hostname or ""
    bare = host.removeprefix("www.")
    out = set()
    for h in p.hrefs:
        u = urllib.parse.urldefrag(urllib.parse.urljoin(base, h.strip()))[0]
        s = urllib.parse.urlsplit(u)
        if s.scheme not in ("http", "https") or (s.hostname or "").removeprefix("www.") != bare:
            continue
        if re.search(r"\.(pdf|zip|gz|jpe?g|png|gif|svg|webp|mp[34]|avi|mov|exe|dmg|apk|css|js|xml|rss|ico)$", s.path, re.I):
            continue
        if u.rstrip("/") != base.rstrip("/"):
            out.add(u)
    if not out:
        return None
    return random.Random(f"{seed}:{id_}").choice(sorted(out))


def html_lang(body):
    m = re.search(rb"<html\b[^>]*\blang\s*=\s*[\"']?([A-Za-z0-9_-]+)", body[:65536], re.I)
    return m.group(1).decode() if m else ""


def keep_headers(headers):
    return [[k, v] for k, v in headers if k.lower() not in HOP]


def do_a2(f, row, out, url=None):
    url = url or row["location"]
    final, status, headers, body = f.get(url, MAX_BYTES["a2"])
    note = row["note"]
    if note.startswith("inner"):
        link = inner_link(final, body, CONF["seeds"]["a2_inner_link"], row["id"])
        if not link:
            raise Unavailable("首页上没有同站链接")
        final, status, headers, body = f.get(link, MAX_BYTES["a2"])
    path = out / "a2" / f"{row['id']}.html"
    path.write_bytes(body)
    path.with_name(path.name + ".headers.json").write_text(json.dumps({"status": status, "headers": keep_headers(headers)}, ensure_ascii=False))
    return final, status, headers, body, path, html_lang(body)


def do_b2(f, row, out):
    url, note = row["location"], row["note"]
    if note == "resolve:govuk":
        _, _, _, body = f.get(url, 2**24)
        doc = json.loads(body)
        atts = doc.get("details", {}).get("attachments", []) or doc.get("details", {}).get("documents", [])
        pdfs = [a.get("url") for a in atts if isinstance(a, dict) and (a.get("content_type") == "application/pdf" or str(a.get("url", "")).lower().endswith(".pdf"))]
        if not pdfs:
            raise Unavailable("页面上没有 PDF 附件")
        url = urllib.parse.urljoin("https://www.gov.uk/", pdfs[0])
    final, status, headers, body = f.get(url, MAX_BYTES["b2"])
    if not body.startswith(b"%PDF"):
        raise Unavailable("不是 PDF（开头不是 %PDF）")
    path = out / "b2" / f"{row['id']}.pdf"
    path.write_bytes(body)
    return final, status, headers, body, path, ""


DNS_NOADDR = re.compile(r"\[Errno -5\]|\[Errno -2\]")
TRANSIENT = re.compile(r"timed out|Temporary failure in name resolution|Connection reset|TimeoutError")
RETRY_WAIT_S = 180


def www_url(url):
    p = urllib.parse.urlsplit(url)
    return urllib.parse.urlunsplit((p.scheme, "www." + p.hostname, p.path, p.query, p.fragment))


def retry_failed(a, rows, out):
    """冻结后补的抓取规则：只补两类失败，不换样本。"""
    path = Path(a.samples) / "a2.fetched.tsv"
    lines = path.read_text(encoding="utf-8").splitlines()
    head, old = lines[0], {l.split("\t", 1)[0]: l.split("\t")[1:] for l in lines[1:] if l}
    todo = {}
    for row in rows:
        r = old[row["id"]]
        if r[0] == "ok":
            continue
        if DNS_NOADDR.search(r[9]) and not urllib.parse.urlsplit(row["location"]).hostname.startswith("www."):
            todo[row["id"]] = (row, "www")
        elif TRANSIENT.search(r[9]):
            todo[row["id"]] = (row, "retry")
    print(f"a2 补抓：裸域名 {sum(k == 'www' for _, k in todo.values())} 份，暂时性错误 {sum(k == 'retry' for _, k in todo.values())} 份", flush=True)
    warc = open(out / "a2" / "snapshots.warc.gz", "ab")
    f = Fetcher(warc)
    pending, attempt = dict(todo), 0
    while pending and attempt <= 2:
        if attempt:
            print(f"a2 补抓：等 {RETRY_WAIT_S} s 后第 {attempt} 次重试 {len(pending)} 份", flush=True)
            time.sleep(RETRY_WAIT_S)

        def one(item):
            id_, (row, kind) = item
            at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
            how = ("www 回退" if kind == "www" else "") + (f"{'，' if kind == 'www' else ''}重试第 {attempt} 次" if attempt else "")
            try:
                final, status, headers, body, p, lang = do_a2(f, row, out, www_url(row["location"]) if kind == "www" else None)
                ctype = next((v for k, v in headers if k.lower() == "content-type"), "")
                return id_, ["ok", final, str(status), ctype, at, str(len(body)), hashlib.sha256(body).hexdigest(), p.relative_to(out).as_posix(), lang, f"冻结后补抓：{how}"], False
            except Unavailable as e:
                return id_, ["unavailable", "", "", "", at, "", "", "", "", f"冻结后补抓（{how}）仍失败：{e}"], False
            except Exception as e:  # noqa: BLE001
                msg = f"{type(e).__name__}: {e}"[:200]
                return id_, ["unavailable", "", "", "", at, "", "", "", "", f"冻结后补抓（{how}）仍失败：{msg}"], bool(TRANSIENT.search(msg))

        with ThreadPoolExecutor(a.threads) as ex:
            res = list(ex.map(one, pending.items()))
        nxt = {}
        for id_, r, transient in res:
            old[id_] = r
            if transient:
                nxt[id_] = pending[id_]
        pending, attempt = nxt, attempt + 1
    warc.close()
    with open(path, "w", encoding="utf-8") as fo:
        fo.write(head + "\n")
        for row in rows:
            fo.write("\t".join([row["id"], *old[row["id"]]]).replace("\n", " ") + "\n")
    with open(out / "a2.manifest", "w", encoding="utf-8") as fo:
        for row in rows:
            r = old[row["id"]]
            if r[0] == "ok":
                fo.write(f"{row['id']}\t{out / r[7]}\t{r[1]}\n")
    got = sum(1 for i in todo if old[i][0] == "ok")
    total = sum(1 for r in old.values() if r[0] == "ok")
    print(f"a2 补抓：补到 {got}/{len(todo)}，A2 共 {total}/{len(rows)}", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("which", choices=["a2", "b2"])
    ap.add_argument("--samples", default=str(HERE / "samples"))
    ap.add_argument("--out", default=str(Path.home() / "mix-tools/inputs"))
    ap.add_argument("--threads", type=int, default=16)
    ap.add_argument("--retry-failed", action="store_true", help="只用于 a2：按冻结后补的抓取规则重抓两类失败")
    a = ap.parse_args()
    if not CONF.get("frozen"):
        raise SystemExit("规则还没冻结（seeds.toml: frozen = false），不抓。")
    out = Path(a.out)
    (out / a.which).mkdir(parents=True, exist_ok=True)
    lines = (Path(a.samples) / f"{a.which}.tsv").read_text(encoding="utf-8").splitlines()
    cols = lines[0].split("\t")
    rows = [dict(zip(cols, l.split("\t"))) for l in lines[1:] if l]
    if a.retry_failed:
        if a.which != "a2":
            raise SystemExit("--retry-failed 只用于 a2")
        return retry_failed(a, rows, out)
    warc = open(out / a.which / "snapshots.warc.gz", "wb") if a.which == "a2" else None
    f = Fetcher(warc)
    work = do_a2 if a.which == "a2" else do_b2
    results = {}
    done = [0]

    def one(row):
        at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        try:
            final, status, headers, body, path, lang = work(f, row, out)
            ctype = next((v for k, v in headers if k.lower() == "content-type"), "")
            r = ["ok", final, str(status), ctype, at, str(len(body)), hashlib.sha256(body).hexdigest(), path.relative_to(out).as_posix(), lang, ""]
        except Unavailable as e:
            r = ["unavailable", "", "", "", at, "", "", "", "", str(e)]
        except Exception as e:  # noqa: BLE001  网络错误、超时、解析失败都记不可用
            r = ["unavailable", "", "", "", at, "", "", "", "", f"{type(e).__name__}: {e}"[:200]]
        results[row["id"]] = r
        with f.lock:
            done[0] += 1
            if done[0] % 25 == 0:
                print(f"{a.which}: {done[0]}/{len(rows)}", flush=True)

    # 按主机打散：同一主机串行（Fetcher 里有主机锁），不同主机并行
    with ThreadPoolExecutor(a.threads) as ex:
        list(ex.map(one, rows))
    if warc:
        warc.close()
    with open(Path(a.samples) / f"{a.which}.fetched.tsv", "w", encoding="utf-8") as fo:
        fo.write("id\tstatus\tfinal_url\thttp_status\tcontent_type\tfetched_at\tbytes\tsha256\tpath\thtml_lang\treason\n")
        for row in rows:
            fo.write("\t".join([row["id"], *results[row["id"]]]).replace("\n", " ") + "\n")
    with open(out / f"{a.which}.manifest", "w", encoding="utf-8") as fo:
        for row in rows:
            r = results[row["id"]]
            if r[0] == "ok":
                fo.write(f"{row['id']}\t{out / r[7]}\t{r[1] if a.which == 'a2' else ''}\n")
    ok = sum(1 for r in results.values() if r[0] == "ok")
    print(f"{a.which}: 完成 {ok}/{len(rows)}，不可用 {len(rows) - ok}", flush=True)


if __name__ == "__main__":
    main()

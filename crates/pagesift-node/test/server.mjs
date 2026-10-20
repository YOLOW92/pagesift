// 测试用的本地 HTTP 服务器：只监听 127.0.0.1，测试不访问外网。
import { createServer } from 'node:http'

const prose = (n, tag = 'Paragraph') =>
  Array.from(
    { length: n },
    (_, i) =>
      `<p>${tag} ${i} of a normal article, with enough words to read like real prose that someone wrote for people rather than for robots.</p>`,
  ).join('\n')

export const article = `<!DOCTYPE html><html lang="en"><head>
<title>Test Article</title>
<meta name="description" content="A page for testing">
<meta property="og:image" content="https://cdn.example.com/cover.png">
</head><body>
<nav><a href="/">Home</a> <a href="/about">About</a></nav>
<article><h1>Hello pagesift</h1>
<p>First paragraph with a <a href="/next">relative link</a>.</p>
<img src="/img/a.png" alt="A picture">
${prose(20)}
</article>
<footer>Copyright footer text</footer>
</body></html>`

/** 大约 `kb` KB 的正文页，用来制造 CPU 负载。 */
export const heavy = (kb) => `<html><head><title>Heavy</title></head><body><article><h1>Heavy</h1>
${prose(Math.ceil((kb * 1024) / 150), 'Heavy paragraph')}
</article></body></html>`

const challenge = `<!DOCTYPE html><html><head><title>Just a moment...</title></head>
<body><div class="main-wrapper"><h1>example.com</h1>
<h2 id="challenge-running">Checking if the site connection is secure</h2>
<noscript>Enable JavaScript and cookies to continue</noscript></div>
<script>(function(){window._cf_chl_opt={cvId:'3',cType:'managed'};
var a=document.createElement('script');a.src='/cdn-cgi/challenge-platform/h/b/orchestrate/chl_page/v1?ray=1';
document.head.appendChild(a);}());</script></body></html>`

const heavyPage = heavy(300)

export async function startServer(host = '127.0.0.1') {
  const hits = new Map()
  const server = createServer((req, res) => {
    const path = new URL(req.url, 'http://x').pathname
    hits.set(path, (hits.get(path) ?? 0) + 1)
    const html = (status, body, headers = {}) => {
      res.writeHead(status, { 'content-type': 'text/html; charset=utf-8', ...headers })
      res.end(body)
    }
    if (path === '/robots.txt') {
      res.writeHead(200, { 'content-type': 'text/plain' })
      return res.end('User-agent: *\nDisallow: /private\n')
    }
    if (path === '/article') return html(200, article, { etag: '"v1"' })
    if (path === '/etag') {
      if (req.headers['if-none-match'] === '"v1"') {
        res.writeHead(304, { etag: '"v1"' })
        return res.end()
      }
      return html(200, article, { etag: '"v1"' })
    }
    if (path === '/redirect') {
      res.writeHead(302, { location: '/article' })
      return res.end()
    }
    if (path.startsWith('/private')) return html(200, article)
    if (path === '/challenge') return html(403, challenge)
    if (path === '/limited') return html(429, 'slow down', { 'retry-after': '7' })
    if (path === '/slow') {
      const t = setTimeout(() => html(200, article), 5000)
      res.on('close', () => clearTimeout(t))
      return
    }
    if (path === '/echo-headers') {
      return html(200, `<html><body><article><h1>Headers</h1><p>x-test=${req.headers['x-test']}</p></article></body></html>`)
    }
    if (path.startsWith('/heavy')) return html(200, heavyPage)
    const delay = path.match(/^\/delay\/(\d+)\//)
    if (delay) {
      const t = setTimeout(() => html(200, article), Number(delay[1]))
      res.on('close', () => clearTimeout(t))
      return
    }
    html(404, '<html><body><h1>Not found</h1></body></html>')
  })
  await new Promise((resolve) => server.listen(0, host, resolve))
  const base = `http://${host}:${server.address().port}`
  return {
    base,
    hits,
    close: () =>
      new Promise((resolve) => {
        server.closeAllConnections()
        server.close(resolve)
      }),
  }
}

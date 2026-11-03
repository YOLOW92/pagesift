import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { Scraper, Page, CrawlStream, PagesiftError } from '../index.js'
import { startServer, article } from './server.mjs'

let srv
before(async () => {
  srv = await startServer()
})
after(() => srv.close())

const local = () => new Scraper({ allowPrivateNetwork: true, requestIntervalMs: 0, perHostConcurrency: 16 })

test('crawl yields every page and the robots-blocked one as an error', async () => {
  const s = local()
  const stream = s.crawl(`${srv.base}/site/`, { engine: 'http', sitemap: 'skip', limit: 10 })
  assert.ok(stream instanceof CrawlStream)
  assert.equal(stream.summary, null)
  const pages = new Map()
  const errors = new Map()
  for await (const item of stream) {
    if (item.page) {
      assert.ok(item.page instanceof Page)
      assert.equal(item.error, undefined)
      pages.set(item.url, item.page.metadata.title)
    } else {
      errors.set(item.url, item.error.code)
    }
  }
  assert.deepEqual(
    [...pages.keys()].sort(),
    [`${srv.base}/site/`, `${srv.base}/site/a`, `${srv.base}/site/b`],
  )
  assert.equal(pages.get(`${srv.base}/site/a`), 'Site A')
  assert.deepEqual([...errors], [[`${srv.base}/site/secret`, 'ROBOTS_DISALLOWED']])
  assert.equal(stream.pages, 3)
  assert.equal(stream.errors, 1)
  assert.equal(stream.summary.stopReason, 'exhausted')
  assert.equal(stream.summary.pages, 3)
  await s.close()
})

test('crawl stops at the limit', async () => {
  const stream = local().crawl(`${srv.base}/site/`, { engine: 'http', sitemap: 'skip', limit: 2 })
  let n = 0
  for await (const _ of stream) n++
  assert.equal(n, 2)
  assert.equal(stream.summary.stopReason, 'limit_reached')
})

test('breaking out of crawl stops it', async () => {
  const stream = local().crawl(`${srv.base}/site/`, { engine: 'http', sitemap: 'skip', maxConcurrency: 1 })
  for await (const item of stream) {
    assert.equal(item.url, `${srv.base}/site/`)
    break
  }
  // 停止之后迭代直接结束
  const rest = []
  for await (const item of stream) rest.push(item)
  assert.deepEqual(rest, [])
})

test('crawl rejects bad options before starting', () => {
  const s = local()
  const invalid = (e) => e instanceof PagesiftError && e.code === 'INVALID_REQUEST'
  assert.throws(() => s.crawl('ftp://example.com/'), invalid)
  assert.throws(() => s.crawl(`${srv.base}/site/`, { sitemap: 'never' }), invalid)
  assert.throws(() => s.crawl(`${srv.base}/site/`, { adaptive: true }), invalid)
})

test('a bad path pattern fails the crawl', async () => {
  const stream = local().crawl(`${srv.base}/site/`, { engine: 'http', includePaths: ['(['] })
  await assert.rejects(async () => {
    for await (const _ of stream);
  }, /INVALID_REQUEST/)
})

test('map lists the links of the site', async () => {
  const links = await local().map(`${srv.base}/site/`, { sitemap: 'skip' })
  const urls = links.map((l) => l.url)
  assert.ok(urls.includes(`${srv.base}/site/a`), urls.join(' '))
  assert.ok(urls.includes(`${srv.base}/site/b`), urls.join(' '))
  await assert.rejects(local().map('not a url'), (e) => e instanceof PagesiftError && e.code === 'INVALID_REQUEST')
})

test('parse turns a file into a Page', async () => {
  const s = local()
  const page = await s.parse(Buffer.from(article), 'article.html')
  assert.ok(page instanceof Page)
  assert.match(page.markdown, /^# Hello pagesift/m)
  assert.equal(page.metadata.title, 'Test Article')
  assert.equal(page.metadata.sourceURL, 'article.html')
  const based = await s.parse(new TextEncoder().encode(article), 'x', {
    contentType: 'text/html',
    url: 'https://example.com/post/',
    formats: ['links'],
  })
  assert.ok(based.links.includes('https://example.com/next'), based.links.join(' '))
  await assert.rejects(s.parse(Buffer.from('just bytes'), 'blob.bin'), (e) => e instanceof PagesiftError && e.code === 'UNSUPPORTED_OPTION')
  await assert.rejects(s.parse(Buffer.from(article), 'a.html', { engine: 'browser' }), (e) => e instanceof PagesiftError && e.code === 'UNSUPPORTED_OPTION')
})

test('parseStream yields every file with its index', async () => {
  const files = [
    { data: Buffer.from(article), filename: 'a.html' },
    { data: Buffer.from('???'), filename: 'b.bin' },
    { data: Buffer.from(article), filename: 'c.htm' },
  ]
  const items = []
  for await (const item of local().parseStream(files, { concurrency: 2 })) items.push(item)
  items.sort((a, b) => a.index - b.index)
  assert.deepEqual(items.map((i) => i.url), ['a.html', 'b.bin', 'c.htm'])
  assert.match(items[0].page.markdown, /Hello pagesift/)
  assert.equal(items[1].error.code, 'UNSUPPORTED_OPTION')
  assert.ok(items[2].page)
})

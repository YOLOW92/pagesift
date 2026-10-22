import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { Scraper, Page, PagesiftError } from '../index.js'
import { startServer } from './server.mjs'

let srv
before(async () => {
  srv = await startServer()
})
after(() => srv.close())

// 测试服务器在 127.0.0.1 上，需要显式放开内网；限速间隔调成 0 让测试跑得快。
const local = () => new Scraper({ allowPrivateNetwork: true, requestIntervalMs: 0, perHostConcurrency: 16 })

test('scrape returns a Page', async () => {
  const s = local()
  const page = await s.scrape(`${srv.base}/article`, { engine: 'http' })
  assert.ok(page instanceof Page)
  assert.match(page.markdown, /^# Hello pagesift/m)
  assert.equal(page.html, null)
  const m = page.metadata
  assert.equal(m.title, 'Test Article')
  assert.equal(m.statusCode, 200)
  assert.equal(m.sourceURL, `${srv.base}/article`)
  assert.equal(m.url, `${srv.base}/article`)
  assert.match(m.contentType, /text\/html/)
  assert.equal(m.ogImage, 'https://cdn.example.com/cover.png')
  assert.equal(m.pagesift.engine, 'http')
  assert.equal(m.pagesift.etag, '"v1"')
  assert.match(m.pagesift.contentHash, /^[0-9a-f]{64}$/)
  // Full-page fingerprints are opt-in for change tracking; normal scraping skips them.
  assert.equal(m.pagesift.rawHash, undefined)
  assert.equal(m.pagesift.textHash, undefined)
  assert.equal(m.pagesift.contentSha256, undefined)
  assert.equal(typeof m.pagesift.timings.total, 'number')
  await s.close()
})

test('formats select the outputs', async () => {
  const page = await local().scrape(`${srv.base}/article`, { engine: 'http', formats: ['html', 'rawHtml', 'links', 'images'] })
  assert.equal(page.markdown, null)
  assert.match(page.html, /<h1>Hello pagesift<\/h1>/)
  assert.match(page.rawHtml, /<footer>/)
  assert.ok(page.links.includes(`${srv.base}/next`))
  assert.ok(page.images.includes(`${srv.base}/img/a.png`))
})

test('headers are sent', async () => {
  const page = await local().scrape(`${srv.base}/echo-headers`, { engine: 'http', headers: { 'x-test': 'hello' } })
  assert.match(page.markdown, /x-test=hello/)
})

test('redirects are followed and recorded', async () => {
  const m = (await local().scrape(`${srv.base}/redirect`, { engine: 'http' })).metadata
  assert.equal(m.url, `${srv.base}/article`)
  assert.equal(m.sourceURL, `${srv.base}/redirect`)
  assert.deepEqual(m.pagesift.redirectChain, [`${srv.base}/redirect`])
})

test('conditional requests report notModified', async () => {
  const page = await local().scrape(`${srv.base}/etag`, { engine: 'http', ifNoneMatch: '"v1"' })
  assert.equal(page.metadata.statusCode, 304)
  assert.equal(page.metadata.pagesift.notModified, true)
  assert.equal(page.markdown, null)
})

test('scrapeMany keeps input order and isolates failures', async () => {
  const urls = [`${srv.base}/article`, `${srv.base}/private/page`, 'not a url', `${srv.base}/heavy/1`]
  const out = await local().scrapeMany(urls, { engine: 'http', concurrency: 2 })
  assert.deepEqual(out.map((r) => r.index), [0, 1, 2, 3])
  assert.deepEqual(out.map((r) => r.url), urls)
  assert.equal(out[0].page.metadata.title, 'Test Article')
  assert.equal(out[0].error, undefined)
  assert.equal(out[1].page, undefined)
  assert.equal(out[1].error.code, 'ROBOTS_DISALLOWED')
  assert.equal(typeof out[1].error.message, 'string')
  assert.equal(out[2].error.code, 'INVALID_REQUEST')
  assert.equal(out[3].page.metadata.title, 'Heavy')
})

test('scrapeMany honours the concurrency limit', async () => {
  const urls = Array.from({ length: 4 }, (_, i) => `${srv.base}/delay/200/c${i}`)
  const t = performance.now()
  const out = await local().scrapeMany(urls, { engine: 'http', concurrency: 2 })
  const ms = performance.now() - t
  assert.equal(out.filter((r) => r.page).length, 4)
  assert.ok(ms >= 380, `4 pages of 200 ms with concurrency 2 took only ${ms.toFixed(0)} ms`)
})

test('scrapeMany with an empty list', async () => {
  assert.deepEqual(await local().scrapeMany([]), [])
})

test('scrapeMany rejects the whole batch on invalid options', async () => {
  await assert.rejects(local().scrapeMany([`${srv.base}/article`], { formats: ['screenshot'] }), (e) => e.code === 'UNSUPPORTED_OPTION')
})

test('scrapeStream yields results in completion order', async () => {
  const urls = [`${srv.base}/delay/400/slow`, `${srv.base}/delay/0/a`, `${srv.base}/delay/50/b`, 'not a url']
  const seen = []
  for await (const item of local().scrapeStream(urls, { engine: 'http' })) seen.push(item)
  assert.equal(seen.length, 4)
  assert.deepEqual(seen.map((r) => r.index).sort(), [0, 1, 2, 3])
  assert.equal(seen.at(-1).index, 0, 'the slow page comes last')
  assert.equal(seen.find((r) => r.index === 3).error.code, 'INVALID_REQUEST')
  assert.equal(seen.find((r) => r.index === 1).page.metadata.title, 'Test Article')
})

test('breaking out of scrapeStream cancels the remaining work', async () => {
  const urls = Array.from({ length: 20 }, (_, i) => `${srv.base}/delay/${i === 0 ? 0 : 300}/cancel${i}`)
  for await (const item of local().scrapeStream(urls, { engine: 'http', concurrency: 2 })) {
    assert.equal(item.index, 0)
    break
  }
  await new Promise((r) => setTimeout(r, 800))
  const started = [...srv.hits.keys()].filter((k) => k.includes('/cancel')).length
  assert.ok(started < 8, `${started} of 20 requests were started after break`)
})

test('a closed Scraper refuses new work', async () => {
  const s = local()
  await s.close()
  await s.close()
  const closed = (e) => e instanceof PagesiftError && e.code === 'INVALID_REQUEST'
  await assert.rejects(s.scrape(`${srv.base}/article`), closed)
  await assert.rejects(s.scrapeMany([`${srv.base}/article`]), closed)
  assert.throws(() => s.scrapeStream([`${srv.base}/article`]), closed)
})

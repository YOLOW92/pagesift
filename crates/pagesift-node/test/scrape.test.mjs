import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { Scraper, PagesiftError } from '../index.js'
import { startServer } from './server.mjs'

let srv
before(async () => {
  srv = await startServer()
})
after(() => srv.close())

// 测试服务器在 127.0.0.1 上，需要显式放开内网；限速间隔调成 0 让测试跑得快。
const local = () => new Scraper({ allowPrivateNetwork: true, requestIntervalMs: 0, perHostConcurrency: 8 })

test('scrape returns a Firecrawl-shaped document', async () => {
  const s = local()
  const doc = await s.scrape(`${srv.base}/article`, { engine: 'http' })
  assert.match(doc.markdown, /^# Hello pagesift/m)
  assert.doesNotMatch(doc.markdown, /Copyright footer/)
  assert.equal(doc.html, undefined)
  const m = doc.metadata
  assert.equal(m.title, 'Test Article')
  assert.equal(m.statusCode, 200)
  assert.equal(m.sourceURL, `${srv.base}/article`)
  assert.equal(m.url, `${srv.base}/article`)
  assert.match(m.contentType, /text\/html/)
  assert.equal(m.ogImage, 'https://cdn.example.com/cover.png')
  assert.equal(m.pagesift.engine, 'http')
  assert.equal(m.pagesift.etag, '"v1"')
  assert.match(m.pagesift.contentSha256, /^[0-9a-f]{64}$/)
  assert.equal(typeof m.pagesift.timings.total, 'number')
  await s.close()
})

test('formats select the outputs', async () => {
  const s = local()
  const doc = await s.scrape(`${srv.base}/article`, {
    engine: 'http',
    formats: ['html', 'rawHtml', { type: 'links' }, 'images'],
  })
  assert.equal(doc.markdown, undefined)
  assert.match(doc.html, /<h1>Hello pagesift<\/h1>/)
  assert.match(doc.rawHtml, /<footer>/)
  assert.ok(doc.links.includes(`${srv.base}/next`), doc.links.join(' '))
  assert.ok(doc.images.includes(`${srv.base}/img/a.png`), doc.images.join(' '))
})

test('headers are sent', async () => {
  const doc = await local().scrape(`${srv.base}/echo-headers`, { engine: 'http', headers: { 'x-test': 'hello' } })
  assert.match(doc.markdown, /x-test=hello/)
})

test('redirects are followed and recorded', async () => {
  const doc = await local().scrape(`${srv.base}/redirect`, { engine: 'http' })
  assert.equal(doc.metadata.url, `${srv.base}/article`)
  assert.equal(doc.metadata.sourceURL, `${srv.base}/redirect`)
  assert.deepEqual(doc.metadata.pagesift.redirectChain, [`${srv.base}/redirect`])
})

test('conditional requests report notModified', async () => {
  const doc = await local().scrape(`${srv.base}/etag`, { engine: 'http', ifNoneMatch: '"v1"' })
  assert.equal(doc.metadata.statusCode, 304)
  assert.equal(doc.metadata.pagesift.notModified, true)
  assert.equal(doc.markdown, undefined)
})

test('scrapeMany keeps input order and isolates failures', async () => {
  const s = local()
  const urls = [`${srv.base}/article`, `${srv.base}/private/page`, 'not a url', `${srv.base}/heavy/1`]
  const out = await s.scrapeMany(urls, { engine: 'http', concurrency: 2 })
  assert.equal(out.length, 4)
  assert.equal(out[0].metadata.title, 'Test Article')
  assert.deepEqual(
    { success: out[1].success, url: out[1].url, code: out[1].code },
    { success: false, url: urls[1], code: 'ROBOTS_DISALLOWED' },
  )
  assert.equal(typeof out[1].error, 'string')
  assert.equal(out[2].code, 'INVALID_REQUEST')
  assert.equal(out[3].metadata.title, 'Heavy')
})

test('scrapeMany honours the concurrency limit', async () => {
  const s = local()
  const urls = Array.from({ length: 6 }, (_, i) => `${srv.base}/heavy/c${i}`)
  const out = await s.scrapeMany(urls, { engine: 'http', concurrency: 1 })
  assert.equal(out.filter((d) => d.metadata?.title === 'Heavy').length, 6)
})

test('scrapeMany with an empty list', async () => {
  assert.deepEqual(await local().scrapeMany([]), [])
})

test('scrapeMany rejects the whole batch on invalid options', async () => {
  await assert.rejects(local().scrapeMany([`${srv.base}/article`], { formats: ['screenshot'] }), (e) => e.code === 'UNSUPPORTED_OPTION')
})

test('a closed Scraper refuses new work', async () => {
  const s = local()
  await s.close()
  await s.close()
  await assert.rejects(s.scrape(`${srv.base}/article`), (e) => e instanceof PagesiftError && e.code === 'INVALID_REQUEST')
})

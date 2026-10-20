import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { Scraper, PagesiftError } from '../index.js'
import { startServer } from './server.mjs'

let srv
before(async () => {
  srv = await startServer()
})
after(() => srv.close())

// 每个用例用新的 Scraper：失败会计入主机的冷却计数，互相不影响。
const local = () => new Scraper({ allowPrivateNetwork: true, requestIntervalMs: 0 })

const rejectsWith = (promise, code, check = () => true) =>
  assert.rejects(promise, (e) => {
    assert.ok(e instanceof PagesiftError, `not a PagesiftError: ${e}`)
    assert.ok(e instanceof Error)
    assert.equal(e.name, 'PagesiftError')
    assert.equal(e.code, code, e.message)
    assert.equal(typeof e.message, 'string')
    assert.ok(e.stack.includes('PagesiftError'))
    return check(e)
  })

test('private addresses are refused by default (SSRF_BLOCKED)', async () => {
  const s = new Scraper()
  await rejectsWith(s.scrape(`${srv.base}/article`), 'SSRF_BLOCKED')
  const [r] = await s.scrapeMany([`${srv.base}/article`])
  assert.equal(r.error.code, 'SSRF_BLOCKED')
})

test('robots.txt is respected (ROBOTS_DISALLOWED)', async () => {
  await rejectsWith(local().scrape(`${srv.base}/private/x`, { engine: 'http' }), 'ROBOTS_DISALLOWED')
  assert.equal(srv.hits.get('/private/x'), undefined)
})

test('ignoreRobotsTxt skips the robots check for a single page', async () => {
  const page = await local().scrape(`${srv.base}/private/y`, { engine: 'http', ignoreRobotsTxt: true })
  assert.equal(page.metadata.title, 'Test Article')
})

test('access barriers are reported with details, not bypassed (ACCESS_BARRIER)', async () => {
  await rejectsWith(local().scrape(`${srv.base}/challenge`), 'ACCESS_BARRIER', (e) => {
    assert.equal(e.details.vendor, 'cloudflare')
    assert.equal(e.details.kind, 'challenge')
    assert.ok(e.details.confidence > 0.5)
    return true
  })
  const [r] = await local().scrapeMany([`${srv.base}/challenge`])
  assert.equal(r.error.code, 'ACCESS_BARRIER')
  assert.equal(r.error.details.vendor, 'cloudflare')
})

test('429 maps to RATE_LIMITED with retryAfter', async () => {
  await rejectsWith(local().scrape(`${srv.base}/limited`, { engine: 'http' }), 'RATE_LIMITED', (e) => e.retryAfter === 7)
  const [r] = await local().scrapeMany([`${srv.base}/limited`], { engine: 'http' })
  assert.equal(r.error.code, 'RATE_LIMITED')
  assert.equal(r.error.retryAfter, 7)
})

test('timeouts map to TIMEOUT', async () => {
  await rejectsWith(local().scrape(`${srv.base}/slow`, { engine: 'http', timeout: 1000 }), 'TIMEOUT')
})

test('invalid input maps to INVALID_REQUEST', async () => {
  const s = local()
  await rejectsWith(s.scrape('ftp://example.com/'), 'INVALID_REQUEST')
  await rejectsWith(s.scrape('not a url'), 'INVALID_REQUEST')
  await rejectsWith(s.scrape(`${srv.base}/article`, { timeout: 500 }), 'INVALID_REQUEST')
  await rejectsWith(s.scrape(`${srv.base}/article`, { timeout: 2000, waitFor: 1500 }), 'INVALID_REQUEST')
  await rejectsWith(s.scrape(`${srv.base}/article`, { engine: 'turbo' }), 'INVALID_REQUEST')
})

test('unsupported formats map to UNSUPPORTED_OPTION', async () => {
  await rejectsWith(local().scrape(`${srv.base}/article`, { formats: ['screenshot'] }), 'UNSUPPORTED_OPTION')
})

test('wrong argument types throw before any work starts', () => {
  assert.throws(() => local().scrape(42))
  assert.throws(() => new Scraper({ requestIntervalMs: 'fast' }))
})

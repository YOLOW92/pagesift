import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { Scraper, convert, convertAsync } from '../index.js'
import { startServer, heavy } from './server.mjs'

let srv
before(async () => {
  srv = await startServer()
})
after(() => srv.close())

/** 运行 `work` 期间每 10 ms 触发一次 setTimeout，记录最长的一次间隔。 */
async function measureLag(work) {
  let last = performance.now()
  let maxGap = 0
  let ticks = 0
  let running = true
  const tick = () => {
    const now = performance.now()
    maxGap = Math.max(maxGap, now - last)
    last = now
    ticks++
    if (running) setTimeout(tick, 10)
  }
  setTimeout(tick, 10)
  const started = performance.now()
  await work()
  const elapsed = performance.now() - started
  // 让被耽误的那次计时器先触发，再停止
  await new Promise((r) => setTimeout(r, 20))
  running = false
  return { maxGap, ticks, elapsed }
}

const MAX_GAP_MS = 150

test('control: synchronous convert does delay timers', async () => {
  const html = heavy(400)
  const { maxGap } = await measureLag(async () => {
    await new Promise((r) => setTimeout(r, 15))
    const t = performance.now()
    while (performance.now() - t < MAX_GAP_MS * 2) convert(html)
  })
  assert.ok(maxGap >= MAX_GAP_MS, `measured only ${maxGap.toFixed(1)} ms`)
})

test('convertAsync keeps the event loop responsive', async () => {
  const html = heavy(400)
  const t0 = performance.now()
  convert(html)
  const once = performance.now() - t0
  const n = 24
  const { maxGap, ticks, elapsed } = await measureLag(() => Promise.all(Array.from({ length: n }, () => convertAsync(html))))
  // 至少是一次同步转换耗时的几倍，测量才有意义
  assert.ok(elapsed > once * 2, `elapsed ${elapsed} vs single ${once}`)
  assert.ok(maxGap < MAX_GAP_MS, `timer delayed ${maxGap.toFixed(1)} ms (single convert ${once.toFixed(1)} ms)`)
  assert.ok(ticks >= elapsed / 10 / 3, `only ${ticks} ticks in ${elapsed.toFixed(0)} ms`)
})

test('scrapeMany keeps the event loop responsive', async () => {
  const s = new Scraper({ allowPrivateNetwork: true, requestIntervalMs: 0, perHostConcurrency: 16 })
  const urls = Array.from({ length: 40 }, (_, i) => `${srv.base}/heavy/${i}`)
  const { maxGap, ticks, elapsed } = await measureLag(async () => {
    const out = await s.scrapeMany(urls, { engine: 'http', concurrency: 16 })
    assert.equal(out.filter((d) => d.metadata?.title === 'Heavy').length, urls.length)
  })
  assert.ok(maxGap < MAX_GAP_MS, `timer delayed ${maxGap.toFixed(1)} ms over ${elapsed.toFixed(0)} ms`)
  assert.ok(ticks >= elapsed / 10 / 3, `only ${ticks} ticks in ${elapsed.toFixed(0)} ms`)
  await s.close()
})

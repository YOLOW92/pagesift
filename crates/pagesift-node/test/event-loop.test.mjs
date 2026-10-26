import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { Scraper, convert, convertAsync, convertMany } from '../index.js'
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

// Intel macOS runners can stall for a few hundred ms under load; 150 ms was too tight
// and failed the release matrix intermittently. Keep the check meaningful but tolerant.
const MAX_GAP_MS = 300
const html = heavy(400)

const responsive = ({ maxGap, ticks, elapsed }) => {
  assert.ok(maxGap < MAX_GAP_MS, `timer delayed ${maxGap.toFixed(1)} ms over ${elapsed.toFixed(0)} ms`)
  assert.ok(ticks >= elapsed / 10 / 3, `only ${ticks} ticks in ${elapsed.toFixed(0)} ms`)
}

test('control: synchronous convert does delay timers', async () => {
  const { maxGap } = await measureLag(async () => {
    await new Promise((r) => setTimeout(r, 15))
    const t = performance.now()
    while (performance.now() - t < MAX_GAP_MS * 2) convert(html)
  })
  assert.ok(maxGap >= MAX_GAP_MS, `measured only ${maxGap.toFixed(1)} ms`)
})

test('convertMany keeps the event loop responsive', async (t) => {
  const t0 = performance.now()
  convert(html)
  const once = performance.now() - t0
  let submitMs = 0
  const r = await measureLag(async () => {
    const started = performance.now()
    const pending = convertMany(Array.from({ length: 64 }, () => html))
    submitMs = performance.now() - started
    const pages = await pending
    // 只读一个字段：其余字段不会复制到 JS
    assert.equal(pages.filter((p) => p.metadata.title === 'Heavy').length, 64)
  })
  t.diagnostic(JSON.stringify({ submitMs, ...r }))
  assert.ok(r.elapsed > once * 2, `elapsed ${r.elapsed} vs single ${once}`)
  responsive(r)
})

test('convertAsync keeps the event loop responsive', async () => {
  responsive(await measureLag(() => Promise.all(Array.from({ length: 32 }, () => convertAsync(html)))))
})

test('scrapeMany and scrapeStream keep the event loop responsive', async () => {
  const s = new Scraper({ allowPrivateNetwork: true, requestIntervalMs: 0, perHostConcurrency: 16 })
  const urls = Array.from({ length: 40 }, (_, i) => `${srv.base}/heavy/${i}`)
  responsive(
    await measureLag(async () => {
      const out = await s.scrapeMany(urls, { engine: 'http' })
      assert.equal(out.filter((r) => r.page).length, urls.length)
    }),
  )
  responsive(
    await measureLag(async () => {
      let n = 0
      for await (const r of s.scrapeStream(urls, { engine: 'http' })) n += r.page ? 1 : 0
      assert.equal(n, urls.length)
    }),
  )
  await s.close()
})

// 在同一批 HTML 上对比 pagesift 的 Node 包和 Readability + Turndown（JSDOM 解析计入耗时）。
//
//   node crates/pagesift-node/bench/bench.mjs bench/data/inputs/manifest.jsonl <装了 jsdom、@mozilla/readability、turndown 的目录> [--out dir]
//
// 在仓库根目录运行：manifest 用 `pagesift bench --emit-inputs` 生成，里面的路径相对仓库根目录（见 bench/compare/README.md）。
// 每个工具在单独的子进程里跑，峰值内存互不影响。--out 时写出 `pagesift bench --external` 用的 JSON lines。

import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { availableParallelism } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const args = process.argv.slice(2)
const self = fileURLToPath(import.meta.url)

const loadPages = (manifest) =>
  readFileSync(manifest, 'utf8')
    .split('\n')
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l))
    .map((r) => ({ id: r.id, html: readFileSync(r.path, 'utf8'), url: r.base_url || r.url || 'https://example.com/' }))

const pct = (sorted, p) => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))]

async function worker(tool, manifest, nodeDir, outPath) {
  const pages = loadPages(manifest)
  let convert
  let parallel = null
  if (tool === 'pagesift') {
    const ps = await import(pathToFileURL(join(self, '../../index.js')).href)
    convert = (html, url) => ps.convert(html, { url }).markdown
    parallel = (items) => Promise.all(items.map((p) => ps.convertAsync(p.html, { url: p.url })))
  } else {
    const load = (p) => import(pathToFileURL(join(nodeDir, 'node_modules', p)).href)
    const { JSDOM, VirtualConsole } = await load('jsdom/lib/api.js')
    const { Readability } = (await load('@mozilla/readability/index.js')).default
    const TurndownService = (await load('turndown/lib/turndown.cjs.js')).default
    const td = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced' })
    const quiet = new VirtualConsole()
    convert = (html, url) => {
      const dom = new JSDOM(html, { url, virtualConsole: quiet })
      const article = new Readability(dom.window.document).parse()
      dom.window.close()
      if (!article) return ''
      const body = td.turndown(article.content || '')
      return article.title ? `# ${article.title}\n\n${body}` : body
    }
  }

  // 预热一遍，排除 JIT 和首次加载
  for (const p of pages.slice(0, 20)) convert(p.html, p.url)

  const lat = []
  const out = []
  for (const p of pages) {
    const t = performance.now()
    let md = ''
    try {
      md = convert(p.html, p.url)
    } catch (e) {
      console.error(`${tool} ${p.id}: ${e.message}`)
    }
    const ms = performance.now() - t
    lat.push(ms)
    out.push(JSON.stringify({ id: p.id, markdown: md, ms: Math.round(ms * 1000) / 1000 }))
  }
  if (outPath) writeFileSync(outPath, out.join('\n') + '\n')
  const sorted = [...lat].sort((a, b) => a - b)
  const total = lat.reduce((a, b) => a + b, 0)

  // 吞吐：pagesift 用 convertAsync 把整批丢给 libuv 线程池；Readability 只能在主线程上跑
  let pagesPerSec = pages.length / (total / 1000)
  if (parallel) {
    const bySize = [...pages].sort((a, b) => b.html.length - a.html.length)
    let best = Infinity
    for (let i = 0; i < 2; i++) {
      const t = performance.now()
      await parallel(bySize)
      best = Math.min(best, performance.now() - t)
    }
    pagesPerSec = pages.length / (best / 1000)
  }
  const rss = process.resourceUsage().maxRSS / 1024
  console.log(JSON.stringify({ tool, pages: pages.length, p50: pct(sorted, 0.5), p95: pct(sorted, 0.95), mean: total / pages.length, total, pagesPerSec, rss }))
}

function main() {
  const outIdx = args.indexOf('--out')
  const outDir = outIdx >= 0 ? args.splice(outIdx, 2)[1] : null
  const [manifest, nodeDir] = args
  if (!manifest || !nodeDir) {
    console.error('usage: node bench/bench.mjs <manifest.jsonl> <node_modules parent dir> [--out dir]')
    process.exit(2)
  }
  const threads = availableParallelism()
  const results = {}
  for (const tool of ['pagesift', 'readability']) {
    const env = { ...process.env, UV_THREADPOOL_SIZE: String(threads) }
    const outPath = outDir ? join(outDir, `node-${tool}.jsonl`) : ''
    const line = execFileSync(process.execPath, [self, '--worker', tool, manifest, nodeDir, outPath], {
      env,
      maxBuffer: 1 << 26,
      stdio: ['ignore', 'pipe', 'inherit'],
    })
    results[tool] = JSON.parse(line.toString().trim().split('\n').pop())
  }
  const a = results.pagesift
  const b = results.readability
  const f = (x) => x.toFixed(2)
  const rows = [
    ['单页耗时 p50，ms（越低越好）', f(a.p50), f(b.p50), `${(b.p50 / a.p50).toFixed(1)}× 更快`],
    ['单页耗时 p95，ms（越低越好）', f(a.p95), f(b.p95), `${(b.p95 / a.p95).toFixed(1)}× 更快`],
    ['单页耗时均值，ms（越低越好）', f(a.mean), f(b.mean), `${(b.mean / a.mean).toFixed(1)}× 更快`],
    [`吞吐，页/秒（越高越好；pagesift 用 convertAsync + ${threads} 个 libuv 线程，Readability 只能单线程）`, a.pagesPerSec.toFixed(1), b.pagesPerSec.toFixed(1), `${(a.pagesPerSec / b.pagesPerSec).toFixed(1)}×`],
    ['进程峰值内存 RSS，MB（越低越好）', a.rss.toFixed(0), b.rss.toFixed(0), `低 ${((1 - a.rss / b.rss) * 100).toFixed(0)}%`],
  ]
  console.log(`\n${a.pages} 个页面，Node ${process.version}\n`)
  console.log('| 指标 | pagesift (Node) | Readability + Turndown | 对比 |')
  console.log('|---|---|---|---|')
  for (const r of rows) console.log(`| ${r.join(' | ')} |`)
}

if (args[0] === '--worker') {
  const [, tool, manifest, nodeDir, outPath] = args
  await worker(tool, manifest, nodeDir, outPath)
} else {
  main()
}

// pagesift Node 包的基准，结果写进 README。两部分：
//
//   提取（评测集 HTML，和 Readability + Turndown 对比，在仓库根目录运行）：
//     node crates/pagesift-node/bench/bench.mjs extract bench/data/inputs/manifest.jsonl <装了 jsdom、@mozilla/readability、turndown 的目录>
//   抓取（本地 HTTP 服务器，在子进程里跑，不访问外网）：
//     node crates/pagesift-node/bench/bench.mjs scrape
//
// manifest 由 `pagesift bench --emit-inputs` 生成，里面的路径相对仓库根目录（见 bench/compare/README.md）。
// 每个工具在单独的子进程里跑，峰值内存互不影响。

import { execFileSync, fork } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { availableParallelism } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const self = fileURLToPath(import.meta.url)
const pkg = () => import(pathToFileURL(join(self, '../../index.js')).href)
const pct = (sorted, p) => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))]
const best = async (runs, fn) => {
  let ms = Infinity
  for (let i = 0; i < runs; i++) {
    const t = performance.now()
    await fn()
    ms = Math.min(ms, performance.now() - t)
  }
  return ms
}
const table = (head, rows) => {
  console.log(`| ${head.join(' | ')} |`)
  console.log(`|${head.map(() => '---').join('|')}|`)
  for (const r of rows) console.log(`| ${r.join(' | ')} |`)
  console.log()
}

const loadPages = (manifest) =>
  readFileSync(manifest, 'utf8')
    .split('\n')
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l))
    .map((r) => ({ id: r.id, html: readFileSync(r.path, 'utf8'), url: r.base_url || r.url || 'https://example.com/' }))

async function readabilityConvert(nodeDir) {
  const load = (p) => import(pathToFileURL(join(nodeDir, 'node_modules', p)).href)
  const { JSDOM, VirtualConsole } = await load('jsdom/lib/api.js')
  const { Readability } = (await load('@mozilla/readability/index.js')).default
  const TurndownService = (await load('turndown/lib/turndown.cjs.js')).default
  const td = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced' })
  const quiet = new VirtualConsole()
  return (html, url) => {
    const dom = new JSDOM(html, { url, virtualConsole: quiet })
    const article = new Readability(dom.window.document).parse()
    dom.window.close()
    if (!article) return ''
    const body = td.turndown(article.content || '')
    return article.title ? `# ${article.title}\n\n${body}` : body
  }
}

/** 子进程：逐页同步转换的耗时；pagesift 再测几种批量方式和按需取字段。 */
async function extractWorker(tool, manifest, nodeDir) {
  const pages = loadPages(manifest)
  const ps = tool === 'pagesift' ? await pkg() : null
  const convert = ps ? (html, url) => ps.convert(html, { url }).markdown : await readabilityConvert(nodeDir)
  for (const p of pages.slice(0, 20)) convert(p.html, p.url)

  const lat = []
  for (const p of pages) {
    const t = performance.now()
    try {
      convert(p.html, p.url)
    } catch (e) {
      console.error(`${tool} ${p.id}: ${e.message}`)
    }
    lat.push(performance.now() - t)
  }
  const sorted = [...lat].sort((a, b) => a - b)
  const total = lat.reduce((a, b) => a + b, 0)
  const out = { tool, pages: pages.length, p50: pct(sorted, 0.5), p95: pct(sorted, 0.95), mean: total / pages.length, syncPps: pages.length / (total / 1000) }

  if (ps) {
    const htmls = pages.map((p) => p.html)
    // 批量：逐个 convertAsync（每页一个 Promise）对比 convertMany（整批一次）
    let e0 = performance.eventLoopUtilization()
    out.asyncPps = pages.length / ((await best(3, () => Promise.all(pages.map((p) => ps.convertAsync(p.html, { url: p.url }))))) / 1000)
    out.asyncElu = performance.eventLoopUtilization(e0).utilization
    e0 = performance.eventLoopUtilization()
    out.manyPps = pages.length / ((await best(3, () => ps.convertMany(htmls))) / 1000)
    out.manyElu = performance.eventLoopUtilization(e0).utilization
    // 字节输入（比如 fs.readFile 的 Buffer）：主线程上没有字符串转码
    const bufs = htmls.map((h) => Buffer.from(h))
    e0 = performance.eventLoopUtilization()
    out.bufPps = pages.length / ((await best(3, () => ps.convertMany(bufs))) / 1000)
    out.bufElu = performance.eventLoopUtilization(e0).utilization
    // 按需取字段：同一批 Page（四种格式都算了），只读 markdown 对比全部取出
    const all = await ps.convertMany(htmls, { formats: ['markdown', 'html', 'links', 'images'] })
    let sink = 0
    out.readMarkdownMs = await best(5, () => {
      for (const p of all) sink += p.markdown.length
    })
    out.readAllMs = await best(5, () => {
      for (const p of all) sink += JSON.stringify(p.toJSON()).length
    })
    // 只算需要的格式：默认（markdown）对比四种格式都算
    out.formatsMdPps = out.manyPps
    out.formatsAllPps = pages.length / ((await best(3, () => ps.convertMany(htmls, { formats: ['markdown', 'html', 'links', 'images'] }))) / 1000)
    if (sink < 0) console.error(sink)
  }
  out.rss = process.resourceUsage().maxRSS / 1024
  console.log(JSON.stringify(out))
}

function extract(manifest, nodeDir) {
  const threads = availableParallelism()
  const run = (tool) =>
    JSON.parse(
      execFileSync(process.execPath, [self, 'extract-worker', tool, manifest, nodeDir], {
        maxBuffer: 1 << 26,
        stdio: ['ignore', 'pipe', 'inherit'],
      })
        .toString()
        .trim()
        .split('\n')
        .pop(),
    )
  const a = run('pagesift')
  const b = run('readability')
  const f = (x, d = 2) => x.toFixed(d)
  console.log(`\n${a.pages} 个页面，Node ${process.version}，${threads} 线程\n`)
  table(
    ['指标', 'pagesift', 'Readability + Turndown', '对比'],
    [
      ['单页耗时 p50，ms（越低越好）', f(a.p50), f(b.p50), `快 ${f(b.p50 / a.p50, 0)} 倍`],
      ['单页耗时 p95，ms（越低越好）', f(a.p95), f(b.p95), `快 ${f(b.p95 / a.p95, 0)} 倍`],
      ['单页耗时均值，ms（越低越好）', f(a.mean), f(b.mean), `快 ${f(b.mean / a.mean, 0)} 倍`],
      ['吞吐，页/秒（越高越好；pagesift 用 convertMany + Buffer 输入，Readability 只能单线程）', f(a.bufPps, 0), f(b.syncPps, 1), `${f(a.bufPps / b.syncPps, 0)} 倍`],
      ['进程峰值内存 RSS，MB（越低越好）', f(a.rss, 0), f(b.rss, 0), `低 ${f((1 - a.rss / b.rss) * 100, 0)}%`],
    ],
  )
  table(
    ['设计', '做法', '吞吐，页/秒（越高越好）', '相对', '主线程占用率（越低越好）'],
    [
      ['同步逐页', '`convert` 循环（主线程）', f(a.syncPps, 0), '1×', '100%'],
      ['逐页异步', '`convertAsync` + `Promise.all`', f(a.asyncPps, 0), `${f(a.asyncPps / a.syncPps, 1)}×`, `${f(a.asyncElu * 100, 0)}%`],
      ['批量', '`convertMany`（Rust 侧并行，一次跨边界）', f(a.manyPps, 0), `${f(a.manyPps / a.syncPps, 1)}×`, `${f(a.manyElu * 100, 0)}%`],
      ['批量 + 字节输入', '`convertMany`，输入是 Buffer（不用转码）', f(a.bufPps, 0), `${f(a.bufPps / a.syncPps, 1)}×`, `${f(a.bufElu * 100, 0)}%`],
      ['只算需要的格式', '`convertMany`，formats 四种全算（对照）', f(a.formatsAllPps, 0), `${f(a.formatsAllPps / a.syncPps, 1)}×`, '—'],
    ],
  )
  table(
    ['按需取字段（同一批 Page）', '主线程耗时，ms（越低越好）'],
    [
      ['只读 `page.markdown`', f(a.readMarkdownMs)],
      ['`page.toJSON()` 全部取出', f(a.readAllMs)],
    ],
  )
}

/** 抓取部分：本地服务器在子进程里，主进程只跑 pagesift。 */
async function scrape() {
  const server = fork(self, ['serve'], { stdio: ['ignore', 'inherit', 'inherit', 'ipc'] })
  const bases = await new Promise((resolve) => server.once('message', resolve))
  const base = bases[0]
  const { Scraper } = await pkg()
  const config = { allowPrivateNetwork: true, requestIntervalMs: 0, perHostConcurrency: 64 }
  const opts = { engine: 'http' }
  const n = 2000
  const urls = Array.from({ length: n }, (_, i) => `${bases[i % bases.length]}/article?${i}`)
  const reused = new Scraper(config)
  await reused.scrapeMany(urls.slice(0, 20), opts)

  const sequentialReuse = await best(2, async () => {
    for (const u of urls.slice(0, 100)) await reused.scrape(u, opts)
  })
  const sequentialFresh = await best(2, async () => {
    for (const u of urls.slice(0, 100)) await new Scraper(config).scrape(u, opts)
  })
  // 事件循环占用率（ELU）：主线程忙的时间占比
  const withElu = async (fn) => {
    const e0 = performance.eventLoopUtilization()
    const ms = await best(3, fn)
    return [ms, performance.eventLoopUtilization(e0).utilization]
  }
  const [many, manyElu] = await withElu(() => reused.scrapeMany(urls, { ...opts, concurrency: 64 }))
  const [each, eachElu] = await withElu(() => Promise.all(urls.map((u) => reused.scrape(u, opts))))

  // 一个慢页面混在快页面里：流式接口不用等最慢的那个
  const mixed = [`${base}/delay/1000/slow`, ...Array.from({ length: 99 }, (_, i) => `${base}/delay/20/f${i}`)]
  let t = performance.now()
  await reused.scrapeMany(mixed, opts)
  const manyFirst = performance.now() - t
  t = performance.now()
  let streamFirst = 0
  let fast = 0
  let streamFast = 0
  for await (const item of reused.scrapeStream(mixed, opts)) {
    if (!streamFirst) streamFirst = performance.now() - t
    if (item.index !== 0 && ++fast === 99) streamFast = performance.now() - t
  }
  await reused.close()
  server.kill()

  const f = (x, d = 0) => x.toFixed(d)
  console.log(`\n本地服务器，16 个主机，每页约 3 KB HTML（让固定开销占主要部分），Node ${process.version}\n`)
  table(
    ['设计', '做法', '结果', '相对'],
    [
      ['长期复用 Scraper', '同一个 Scraper 顺序抓 100 页，ms（越低越好）', f(sequentialReuse), '1×'],
      ['', '每页新建 Scraper，ms（越低越好）', f(sequentialFresh), `慢 ${f(sequentialFresh / sequentialReuse, 1)} 倍`],
      ['批量在 Rust 侧调度', `\`scrapeMany\` ${n} 页，页/秒（越高越好）`, f(n / (many / 1000)), '1×'],
      ['', `\`scrape\` × ${n} + \`Promise.all\`，页/秒（越高越好）`, f(n / (each / 1000)), `${f(many / each, 2)}×`],
      ['', '`scrapeMany` 期间主线程占用率（越低越好）', `${f(manyElu * 100)}%`, '—'],
      ['', '`scrape` + `Promise.all` 期间主线程占用率（越低越好）', `${f(eachElu * 100)}%`, '—'],
      ['按完成顺序流式返回', '`scrapeMany` 拿到第一个结果，ms（越低越好）', f(manyFirst), '—'],
      ['', '`scrapeStream` 拿到第一个结果，ms（越低越好）', f(streamFirst), '—'],
      ['', '`scrapeStream` 拿到全部 99 个快页面，ms（越低越好）', f(streamFast), '—'],
    ],
  )
}

const [mode, ...rest] = process.argv.slice(2)
if (mode === 'extract') extract(...rest)
else if (mode === 'extract-worker') await extractWorker(...rest)
else if (mode === 'scrape') await scrape()
else if (mode === 'serve') {
  const { startServer } = await import(pathToFileURL(join(self, '../../test/server.mjs')).href)
  // 16 个回环地址各一个服务器：pagesift 按主机限速，多个主机才能测出调度本身的开销
  const bases = []
  for (let i = 1; i <= 16; i++) bases.push((await startServer(`127.0.0.${i}`)).base)
  process.send(bases)
} else {
  console.error('usage: bench.mjs extract <manifest.jsonl> <node_modules parent dir> | bench.mjs scrape')
  process.exit(2)
}

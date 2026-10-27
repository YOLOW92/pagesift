// Mozilla Readability + Turndown 的常驻 worker，协议见 README.md。
//   MIX_NODE_DIR=~/mix-tools/node/readability node benchmarks/mix/adapters/readability.mjs
// 按 Readability README 的 Node 用法：jsdom 解析 → new Readability(document).parse() → 正文 HTML 交给 Turndown。
// 两者都用默认参数（Turndown 默认标题是 setext 风格，评分脚本两种标题都认）；标题按惯例拼在正文前面。
// 网页按 UTF-8 宽松解码后交给 jsdom，解析时间算在耗时里。任务带原始 URL 就交给 jsdom 解析相对链接，否则用占位地址。
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import { createInterface } from 'node:readline';
import { join } from 'node:path';
import { homedir } from 'node:os';

const dir = process.env.MIX_NODE_DIR || join(homedir(), 'mix-tools/node/readability');
const require = createRequire(join(dir, 'package.json'));
const { JSDOM, VirtualConsole } = require('jsdom');
const { Readability } = require('@mozilla/readability');
const TurndownService = require('turndown');
const version = (p) => require(`${p}/package.json`).version;

const quiet = new VirtualConsole();
const td = new TurndownService();
const send = (o) => process.stdout.write(JSON.stringify(o) + '\n');

function convert(path, url) {
  const html = new TextDecoder('utf-8').decode(readFileSync(path));
  const dom = new JSDOM(html, { url: url || 'https://example.invalid/', virtualConsole: quiet });
  const article = new Readability(dom.window.document).parse();
  dom.window.close();
  if (!article) return '';
  const body = td.turndown(article.content || '');
  return article.title ? `# ${article.title}\n\n${body}` : body;
}

send({ ready: true, tool: 'readability', version: `@mozilla/readability ${version('@mozilla/readability')} + turndown ${version('turndown')} + jsdom ${version('jsdom')}`, pid: process.pid });
for await (const line of createInterface({ input: process.stdin })) {
  if (!line.trim()) continue;
  const job = JSON.parse(line);
  const t = performance.now();
  try {
    const text = convert(job.input, job.url);
    const ms = performance.now() - t;
    writeFileSync(job.out, text);
    send({ id: job.id, ok: true, ms, bytes: Buffer.byteLength(text), error: null });
  } catch (e) {
    send({ id: job.id, ok: false, ms: performance.now() - t, bytes: 0, error: String(e).slice(0, 500) });
  }
}

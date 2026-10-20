// 在 pagesift bench 的输入上跑 JS 同类库，输出 `pagesift bench --external` 用的 JSON lines。
//
//   node bench/compare/js_adapter.mjs <readability|defuddle> <node_modules 所在目录> <manifest.jsonl> <out.jsonl>
//
// 两个工具都需要 DOM，用 JSDOM 解析，解析时间算在耗时里。
// Readability 输出 HTML，按惯常用法接 Turndown 转 Markdown；标题按惯例单独给出，这里拼在正文前面。

import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [tool, nodeDir, manifest, outPath] = process.argv.slice(2);
const load = (p) => import(pathToFileURL(join(nodeDir, 'node_modules', p)).href);

const { JSDOM, VirtualConsole } = await load('jsdom/lib/api.js');
const quiet = new VirtualConsole();

let convert;
if (tool === 'readability') {
  const { Readability } = (await load('@mozilla/readability/index.js')).default;
  const TurndownService = (await load('turndown/lib/turndown.cjs.js')).default;
  const td = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced' });
  convert = async (html, url) => {
    const dom = new JSDOM(html, { url, virtualConsole: quiet });
    const article = new Readability(dom.window.document).parse();
    dom.window.close();
    if (!article) return '';
    const body = td.turndown(article.content || '');
    return article.title ? `# ${article.title}\n\n${body}` : body;
  };
} else if (tool === 'defuddle') {
  const { Defuddle } = await load('defuddle/dist/node.js');
  convert = async (html, url) => {
    const dom = new JSDOM(html, { url, virtualConsole: quiet });
    const r = await Defuddle(dom.window.document, url, { markdown: true });
    dom.window.close();
    return r.title ? `# ${r.title}\n\n${r.content || ''}` : r.content || '';
  };
} else {
  throw new Error('tool must be readability or defuddle');
}

const out = [];
for (const line of readFileSync(manifest, 'utf8').split('\n')) {
  if (!line.trim()) continue;
  const row = JSON.parse(line);
  const html = readFileSync(row.path, 'utf8');
  const url = row.base_url || row.url || 'https://example.com/';
  const t = performance.now();
  let markdown = '';
  try {
    markdown = await convert(html, url);
  } catch (e) {
    console.error(`${row.id}: ${e.message}`);
  }
  out.push(JSON.stringify({ id: row.id, markdown, ms: performance.now() - t }));
}
writeFileSync(outPath, out.join('\n') + '\n');

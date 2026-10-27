// 在 pagesift bench 的输入上跑 JS 同类库，输出 `pagesift bench --external` 用的 JSON lines。
//
//   node benchmarks/web-extraction/compare/js_adapter.mjs <readability|defuddle|article-extractor> <node_modules 所在目录> <manifest.jsonl> <out.jsonl>
//
// 两个工具都需要 DOM，用 JSDOM 解析，解析时间算在耗时里。
// Readability 输出 HTML，按惯常用法接 Turndown 转 Markdown；标题按惯例单独给出，这里拼在正文前面。

import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
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
} else if (tool === 'jina-reader') {
  const { Readability } = (await load('@mozilla/readability/index.js')).default;
  const TurndownService = (await load('turndown/lib/turndown.cjs.js')).default;
  const { gfm } = await load('turndown-plugin-gfm/lib/turndown-plugin-gfm.cjs.js');
  const td = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced', bulletListMarker: '-' });
  td.use(gfm);
  convert = async (html, url) => {
    const dom = new JSDOM(html, { url, virtualConsole: quiet });
    const article = new Readability(dom.window.document).parse();
    dom.window.close();
    if (!article) return '';
    const body = td.turndown(article.content || '');
    return article.title ? `# ${article.title}\n\n${body}` : body;
  };
} else if (tool === 'mdream') {
  const { htmlToMarkdown } = await load('mdream/dist/index.mjs');
  convert = async (html, url) => {
    const str = typeof html === 'string' ? html : new TextDecoder('utf-8', { fatal: false }).decode(html);
    return htmlToMarkdown(str, { origin: url, minimal: true, frontmatter: false }) || '';
  };
} else if (tool === 'article-extractor') {
  // @extractus/article-extractor：自带解析，输出 HTML，同样接 Turndown
  const { extractFromHtml } = await load('@extractus/article-extractor/esm/mod.js');
  const TurndownService = (await load('turndown/lib/turndown.cjs.js')).default;
  const td = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced' });
  convert = async (html, url) => {
    const a = await extractFromHtml(html, url);
    if (!a) return '';
    const body = td.turndown(a.content || '');
    return a.title ? `# ${a.title}\n\n${body}` : body;
  };
} else {
  throw new Error('tool must be readability, defuddle or article-extractor');
}

writeFileSync(outPath, '');
for (const line of readFileSync(manifest, 'utf8').split('\n')) {
  if (!line.trim()) continue;
  const row = JSON.parse(line);
  // PAGESIFT_RAW_INPUT=1：原始字节交给 JSDOM，由它按 BOM 和 <meta> 识别编码
  const html = process.env.PAGESIFT_RAW_INPUT === '1' ? readFileSync(row.path) : readFileSync(row.path, 'utf8');
  const url = row.base_url || row.url || 'https://example.com/';
  const t = performance.now();
  let markdown = '';
  try {
    markdown = await convert(html, url);
  } catch (e) {
    console.error(`${row.id}: ${e.message}`);
  }
  appendFileSync(outPath, JSON.stringify({ id: row.id, markdown, ms: performance.now() - t }) + '\n');
}

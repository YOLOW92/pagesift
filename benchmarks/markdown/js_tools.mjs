// JS 转换器：把目录里的 NNN.html 转成 <输出目录>/<工具>/NNN.md。
//   node js_tools.mjs <node_modules 所在目录> <html 目录> <输出目录> turndown defuddle
// turndown：ATX 标题、围栏代码块（本地没有 GFM 插件，所以不支持表格和删除线，结果里注明）；
// defuddle：markdown: true（它同时做正文提取；测试文档整页都是正文）。
import { readFileSync, writeFileSync, mkdirSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [nodeDir, src, out, ...tools] = process.argv.slice(2);
const load = (p) => import(pathToFileURL(join(nodeDir, 'node_modules', p)).href);
const { JSDOM, VirtualConsole } = await load('jsdom/lib/api.js');
const quiet = new VirtualConsole();
const files = readdirSync(src).filter((f) => f.endsWith('.html')).sort();

for (const tool of tools) {
  let convert;
  if (tool === 'turndown') {
    const TurndownService = (await load('turndown/lib/turndown.cjs.js')).default;
    const td = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced', bulletListMarker: '-' });
    convert = async (html) => td.turndown(html);
  } else if (tool === 'jina-reader' || tool === 'turndown-gfm') {
    const TurndownService = (await load('turndown/lib/turndown.cjs.js')).default;
    const { gfm } = await load('turndown-plugin-gfm/lib/turndown-plugin-gfm.cjs.js');
    const td = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced', bulletListMarker: '-' });
    td.use(gfm);
    convert = async (html) => td.turndown(html);
  } else if (tool === 'mdream') {
    const { htmlToMarkdown } = await load('mdream/dist/index.mjs');
    convert = async (html) => htmlToMarkdown(html) || '';
  } else if (tool === 'defuddle') {
    const { Defuddle } = await load('defuddle/dist/node.js');
    convert = async (html) => {
      const dom = new JSDOM(html, { url: 'https://example.com/', virtualConsole: quiet });
      const r = await Defuddle(dom.window.document, 'https://example.com/', { markdown: true });
      dom.window.close();
      return r.content || '';
    };
  }
  const dir = join(out, tool);
  mkdirSync(dir, { recursive: true });
  let total = 0;
  for (const f of files) {
    const html = readFileSync(join(src, f), 'utf8');
    const t = performance.now();
    let md = '';
    try { md = await convert(html); } catch (e) { console.error(tool, f, e.message); }
    total += performance.now() - t;
    writeFileSync(join(dir, f.replace(/\.html$/, '.md')), md);
  }
  console.log(`${tool}: ${(total / files.length).toFixed(2)} ms/page`);
}

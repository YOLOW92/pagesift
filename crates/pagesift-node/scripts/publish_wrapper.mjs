// 发布 pagesift 无 scope 别名包，指向 @pagesift/node。
// 避免 pagesift-win32-x64-msvc 触发 npm 的无 scope 包垃圾检测拦截。
import { execSync } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

const tmpDir = resolve('/tmp/pagesift-wrapper-pkg')
mkdirSync(tmpDir, { recursive: true })

const pkgJson = {
  name: 'pagesift',
  version: '0.40.0',
  description: 'Turn web pages into clean Markdown for LLMs: a polite, Firecrawl-compatible scraping engine written in Rust.',
  license: 'AGPL-3.0-only',
  repository: {
    type: 'git',
    url: 'git+https://github.com/YOLOW92/pagesift.git',
    directory: 'crates/pagesift-node'
  },
  keywords: [
    'markdown',
    'scraper',
    'crawler',
    'firecrawl',
    'llm',
    'html-to-markdown',
    'napi-rs'
  ],
  main: 'index.js',
  types: 'index.d.ts',
  engines: {
    node: '>= 18'
  },
  dependencies: {
    '@pagesift/node': '0.40.0'
  }
}

writeFileSync(resolve(tmpDir, 'package.json'), JSON.stringify(pkgJson, null, 2) + '\n')
writeFileSync(resolve(tmpDir, 'index.js'), "module.exports = require('@pagesift/node');\n")
writeFileSync(resolve(tmpDir, 'index.d.ts'), "export * from '@pagesift/node';\n")
writeFileSync(resolve(tmpDir, 'README.md'), '# pagesift\n\nOfficial Node.js wrapper for `@pagesift/node`.\n\n```bash\nnpm install pagesift\n# or\nnpm install @pagesift/node\n```\n')

console.log('Publishing pagesift alias wrapper to npm...')
try {
  execSync('npm publish --access public', { cwd: tmpDir, stdio: 'inherit' })
  console.log('pagesift wrapper published successfully.')
} catch (e) {
  console.warn('pagesift wrapper publish skipped or failed:', e.message)
}

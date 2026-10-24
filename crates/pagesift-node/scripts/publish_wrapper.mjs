// 发布 pagesift 无 scope 别名包，指向 @pagesift/node。
// 避免 pagesift-win32-x64-msvc 触发 npm 的无 scope 包垃圾检测拦截。
import { execSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = fileURLToPath(new URL('.', import.meta.url))
const nodePackage = JSON.parse(readFileSync(resolve(here, '..', 'package.json'), 'utf8'))
const version = process.env.PAGESIFT_WRAPPER_VERSION || nodePackage.version
const dryRun = process.argv.includes('--dry-run')

const tmpDir = resolve('/tmp/pagesift-wrapper-pkg')
mkdirSync(tmpDir, { recursive: true })

const pkgJson = {
  name: 'pagesift',
  version,
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
    '@pagesift/node': version
  }
}

writeFileSync(resolve(tmpDir, 'package.json'), JSON.stringify(pkgJson, null, 2) + '\n')
writeFileSync(resolve(tmpDir, 'index.js'), "module.exports = require('@pagesift/node');\n")
writeFileSync(resolve(tmpDir, 'index.d.ts'), "export * from '@pagesift/node';\n")
writeFileSync(resolve(tmpDir, 'README.md'), '# pagesift\n\nOfficial Node.js wrapper for `@pagesift/node`.\n\n```bash\nnpm install pagesift\n# or\nnpm install @pagesift/node\n```\n')

console.log('Publishing pagesift alias wrapper to npm...')
if (dryRun) {
  execSync('npm pack --dry-run', { cwd: tmpDir, stdio: 'inherit' })
} else {
  let published = false
  try {
    const found = execSync(`npm view pagesift@${version} version --json`, {
      cwd: tmpDir,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore']
    })
    published = JSON.parse(found) === version
  } catch {
    // A missing version is expected on the first publish.
  }
  if (published) {
    console.log(`pagesift wrapper ${version} is already published.`)
  } else {
    execSync('npm publish --access public', { cwd: tmpDir, stdio: 'inherit' })
    console.log(`pagesift wrapper ${version} published successfully.`)
  }
}

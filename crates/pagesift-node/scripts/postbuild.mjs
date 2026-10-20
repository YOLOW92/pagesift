// 这两个导出在 `#[napi(module_exports)]` 里设置，napi 生成的 index.js 不会静态列出它们，
// ESM 的具名导入（`import { PagesiftError } from 'pagesift'`）因此找不到。这里补上。
import { readFileSync, writeFileSync } from 'node:fs'

const file = new URL('../index.js', import.meta.url)
const extra = ['PagesiftError', 'version']
let src = readFileSync(file, 'utf8')
for (const name of extra) {
  const line = `module.exports.${name} = nativeBinding.${name}`
  if (!src.includes(line)) src = src.trimEnd() + '\n' + line + '\n'
}
writeFileSync(file, src)

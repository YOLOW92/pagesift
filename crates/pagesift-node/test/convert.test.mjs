import { test } from 'node:test'
import assert from 'node:assert/strict'
import { convert, convertAsync, convertMany, Page, PagesiftError, version } from '../index.js'
import { article } from './server.mjs'

const url = 'https://example.com/post'

test('exports the version', () => {
  assert.match(version, /^\d+\.\d+\.\d+/)
})

test('convert returns a Page with markdown and metadata', () => {
  const page = convert(article, { url })
  assert.ok(page instanceof Page)
  assert.match(page.markdown, /^# Hello pagesift/m)
  assert.match(page.markdown, /\[relative link\]\(https:\/\/example\.com\/next\)/)
  assert.doesNotMatch(page.markdown, /Copyright footer/)
  const m = page.metadata
  assert.equal(m.title, 'Test Article')
  assert.equal(m.description, 'A page for testing')
  assert.equal(m.language, 'en')
  assert.equal(m.ogImage, 'https://cdn.example.com/cover.png')
  assert.equal(m.url, url)
  assert.equal(m.sourceURL, url)
})

test('only the requested formats are computed', () => {
  const md = convert(article, { url })
  assert.equal(md.html, null)
  assert.equal(md.rawHtml, null)
  assert.equal(md.links, null)
  assert.equal(md.images, null)
  const all = convert(article, { url, formats: ['html', 'rawHtml', 'links', 'images'] })
  assert.equal(all.markdown, null)
  assert.match(all.html, /<h1>Hello pagesift<\/h1>/)
  assert.equal(all.rawHtml, article)
  assert.ok(all.links.includes('https://example.com/next'))
  assert.ok(all.images.includes('https://example.com/img/a.png'))
})

test('toJSON returns a plain object with only the present fields', () => {
  const page = convert(article, { url, formats: ['markdown', 'links'] })
  const json = page.toJSON()
  assert.deepEqual(Object.keys(json).sort(), ['links', 'markdown', 'metadata'])
  assert.equal(json.markdown, page.markdown)
  assert.deepEqual(JSON.parse(JSON.stringify(page)), json)
})

test('accepts UTF-8 bytes as input', () => {
  const bytes = new TextEncoder().encode(article)
  assert.deepEqual(convert(bytes, { url }).toJSON(), convert(article, { url }).toJSON())
  assert.deepEqual(convert(Buffer.from('<p>héllo</p>')).markdown.trim(), 'héllo')
})

test('onlyMainContent: false keeps the page chrome', () => {
  assert.match(convert(article, { onlyMainContent: false }).markdown, /Copyright footer/)
})

test('convert without options', () => {
  const page = convert('<p>hi</p>')
  assert.equal(page.markdown.trim(), 'hi')
  assert.deepEqual(page.metadata, {})
})

test('convertAsync gives the same result as convert', async () => {
  assert.deepEqual((await convertAsync(article, { url })).toJSON(), convert(article, { url }).toJSON())
})

test('convertMany keeps input order', async () => {
  const htmls = Array.from({ length: 50 }, (_, i) => `<article><h1>Doc ${i}</h1><p>Body of document ${i}.</p></article>`)
  htmls.push(new TextEncoder().encode(article))
  const pages = await convertMany(htmls, { url })
  assert.equal(pages.length, 51)
  pages.slice(0, 50).forEach((p, i) => assert.match(p.markdown, new RegExp(`^# Doc ${i}$`, 'm')))
  assert.equal(pages[50].metadata.title, 'Test Article')
  assert.deepEqual(await convertMany([]), [])
})

const invalid = (e) => e instanceof PagesiftError && e instanceof Error && e.name === 'PagesiftError' && e.code === 'INVALID_REQUEST'
const unsupported = (e) => e instanceof PagesiftError && e.code === 'UNSUPPORTED_OPTION'

test('an invalid url throws or rejects with PagesiftError', async () => {
  assert.throws(() => convert('<p>x</p>', { url: 'not a url' }), invalid)
  await assert.rejects(convertAsync('<p>x</p>', { url: 'nope' }), invalid)
  await assert.rejects(convertMany(['<p>x</p>'], { url: 'nope' }), invalid)
})

test('an unknown format is UNSUPPORTED_OPTION', async () => {
  assert.throws(() => convert('<p>x</p>', { formats: ['screenshot'] }), unsupported)
  await assert.rejects(convertMany(['<p>x</p>'], { formats: ['screenshot'] }), unsupported)
})

test('convertMany rejects a non-HTML element before doing any work', () => {
  assert.throws(() => convertMany(['<p>a</p>', 42]))
})

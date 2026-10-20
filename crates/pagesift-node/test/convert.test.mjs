import { test } from 'node:test'
import assert from 'node:assert/strict'
import { convert, convertAsync, PagesiftError, version } from '../index.js'
import { article } from './server.mjs'

test('exports the version', () => {
  assert.match(version, /^\d+\.\d+\.\d+/)
})

test('convert keeps the main content and resolves links', () => {
  const doc = convert(article, { url: 'https://example.com/post' })
  assert.match(doc.markdown, /^# Hello pagesift/m)
  assert.match(doc.markdown, /\[relative link\]\(https:\/\/example\.com\/next\)/)
  assert.doesNotMatch(doc.markdown, /Copyright footer/)
  assert.equal(doc.metadata.title, 'Test Article')
  assert.equal(doc.metadata.description, 'A page for testing')
  assert.equal(doc.metadata.language, 'en')
  assert.equal(doc.metadata.ogImage, 'https://cdn.example.com/cover.png')
  assert.equal(doc.metadata.url, 'https://example.com/post')
  assert.equal(doc.metadata.sourceURL, 'https://example.com/post')
})

test('convert with onlyMainContent: false keeps the page chrome', () => {
  const doc = convert(article, { onlyMainContent: false })
  assert.match(doc.markdown, /Copyright footer/)
})

test('convert without options', () => {
  const doc = convert('<p>hi</p>')
  assert.equal(doc.markdown.trim(), 'hi')
  assert.deepEqual(doc.metadata, {})
})

test('convertAsync gives the same result as convert', async () => {
  const opts = { url: 'https://example.com/post' }
  assert.deepEqual(await convertAsync(article, opts), convert(article, opts))
})

test('convert throws PagesiftError on an invalid url', () => {
  assert.throws(
    () => convert('<p>x</p>', { url: 'not a url' }),
    (e) => e instanceof PagesiftError && e instanceof Error && e.code === 'INVALID_REQUEST' && e.name === 'PagesiftError',
  )
})

test('convertAsync rejects with PagesiftError on an invalid url', async () => {
  await assert.rejects(convertAsync('<p>x</p>', { url: 'nope' }), (e) => e instanceof PagesiftError && e.code === 'INVALID_REQUEST')
})

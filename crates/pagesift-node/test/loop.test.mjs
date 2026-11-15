import { test, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Scraper, PagesiftError } from '../index.js'
import { startServer } from './server.mjs'

let srv
before(async () => {
  srv = await startServer()
})
after(() => srv.close())

test('sync source lifecycle and graph queries with persistent storePath', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'pagesift-node-loop-'))
  const s = new Scraper({
    allowPrivateNetwork: true,
    requestIntervalMs: 0,
    perHostConcurrency: 8,
    storePath: dir,
  })
  try {
    const created = await s.sync({
      action: 'create',
      sourceId: 'local-site',
      seeds: [`${srv.base}/site/`],
      maxDepth: 1,
      maxRequestsPerTick: 5,
      allowBrowser: false,
    })
    assert.equal(created.success, true)
    assert.equal(created.source.id, 'local-site')

    const listed = await s.sync({ action: 'list' })
    assert.equal(listed.total, 1)

    const tick = await s.sync({ action: 'run', id: 'local-site', maxRequests: 3 })
    assert.equal(tick.success, true)
    assert.equal(tick.delta.sourceId, 'local-site')
    assert.ok(tick.delta.stats.requestsExecuted >= 1)

    const state = await s.sync({ action: 'state', id: 'local-site' })
    assert.equal(state.success, true)
    assert.equal(state.state.spec.id, 'local-site')

    const deltas = await s.sync({ action: 'deltas', id: 'local-site', limit: 5 })
    assert.equal(deltas.success, true)
    assert.equal(deltas.total, 1)

    const hist = await s.queryGraph({ historyUrl: `${srv.base}/site/` })
    assert.equal(hist.success, true)
    assert.ok(Array.isArray(hist.versions))
    assert.ok(hist.versions.length >= 1)

    const claims = await s.queryGraph({ claims: true, url: `${srv.base}/site/` })
    assert.equal(claims.success, true)
    assert.equal(typeof claims.total, 'number')
    assert.ok(Array.isArray(claims.claims ?? []))

    const del = await s.sync({ action: 'delete', id: 'local-site' })
    assert.equal(del.deleted, true)

    await assert.rejects(
      () => s.sync({ action: 'get', id: 'local-site' }),
      (e) => e instanceof PagesiftError && e.code === 'NOT_FOUND'
    )
  } finally {
    await s.close()
    try {
      rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
    } catch {}
  }
})

test('sync and queryGraph require storePath', async () => {
  const s = new Scraper({ allowPrivateNetwork: true })
  try {
    await assert.rejects(
      () => s.sync({ action: 'list' }),
      (e) => e instanceof PagesiftError && e.code === 'INVALID_REQUEST'
    )
    await assert.rejects(
      () => s.queryGraph({ conflicts: true }),
      (e) => e instanceof PagesiftError && e.code === 'INVALID_REQUEST'
    )
  } finally {
    await s.close()
  }
})

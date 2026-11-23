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
    assert.ok(typeof created.source.instanceId === 'string' && created.source.instanceId.length > 0)

    const preCreateState = await s.sync({
      action: 'state',
      id: 'local-site',
      asOf: created.source.createdAtMs - 1,
    })
    assert.equal(preCreateState.state.historyStatus, 'not_created_yet')
    assert.equal(preCreateState.state.spec ?? null, null)

    const listed = await s.sync({ action: 'list' })
    assert.equal(listed.total, 1)

    const tick = await s.sync({ action: 'run', id: 'local-site', maxRequests: 3 })
    assert.equal(tick.success, true)
    assert.equal(tick.delta.sourceId, 'local-site')
    assert.equal(tick.delta.instanceId, created.source.instanceId)
    assert.ok(tick.delta.stats.requestsExecuted >= 1)

    const state = await s.sync({ action: 'state', id: 'local-site' })
    assert.equal(state.success, true)
    assert.equal(state.state.spec.id, 'local-site')
    assert.equal(state.state.spec.instanceId, created.source.instanceId)

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
    assert.equal(claims.historyUnavailable ?? false, false)
    assert.ok(Array.isArray(claims.claims ?? []))

    const del = await s.sync({ action: 'delete', id: 'local-site' })
    assert.equal(del.deleted, true)

    await assert.rejects(
      () => s.sync({ action: 'get', id: 'local-site' }),
      (e) => e instanceof PagesiftError && e.code === 'NOT_FOUND'
    )

    const recreated = await s.sync({
      action: 'create',
      sourceId: 'local-site',
      seeds: [`${srv.base}/site/`],
      maxDepth: 1,
      maxRequestsPerTick: 5,
      allowBrowser: false,
    })
    assert.equal(recreated.success, true)
    assert.notEqual(recreated.source.instanceId, created.source.instanceId)
    const recreatedDeltas = await s.sync({ action: 'deltas', id: 'local-site', limit: 5 })
    assert.equal(recreatedDeltas.total, 0)
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

test('context supports inline compilation and persistent units/deltas/action', async () => {
  const stateless = new Scraper()
  try {
    const inline = await stateless.context({
      action: 'pack',
      query: 'rate limit pro tier',
      budgetTokens: 256,
      documents: [
        {
          url: 'https://docs.example.com/limits',
          markdown: '# API Quotas\n\n| Tier | RPM |\n|---|---|\n| Pro | 600 |\n',
        },
      ],
    })
    assert.equal(inline.success, true)
    assert.ok(inline.pack.provenanceManifest.length >= 1)
    assert.ok(inline.pack.compiledContext.includes('Tier'))
  } finally {
    await stateless.close()
  }

  const dir = mkdtempSync(join(tmpdir(), 'pagesift-node-ctx-'))
  const s = new Scraper({ storePath: dir })
  try {
    const ingested = await s.context({
      action: 'ingest',
      sourceId: 'docs',
      documents: [
        {
          url: 'https://docs.example.com/pricing',
          markdown: '# Enterprise Plan\n\nPrice: $49 per seat unless annual billing applies.\n',
        },
      ],
    })
    assert.equal(ingested.success, true)
    assert.ok(ingested.liveUnitCount >= 1)

    const searched = await s.context({
      action: 'search',
      query: 'Enterprise seat price',
      sourceId: 'docs',
    })
    assert.equal(searched.success, true)
    assert.ok(searched.report.hits.length >= 1)

    const polled = await s.context({
      action: 'deltas',
      sourceId: 'docs',
      consumerId: 'node-worker',
    })
    assert.equal(polled.success, true)
    assert.equal(polled.poll.deltas.length, 1)

    const acked = await s.context({
      action: 'ack',
      sourceId: 'docs',
      consumerId: 'node-worker',
      instanceId: ingested.delta.instanceId,
      ackedDeltaSeq: 1,
    })
    assert.equal(acked.success, true)
    assert.equal(acked.checkpoint.ackedDeltaSeq, 1)

    const act = await s.context({
      action: 'action',
      mode: 'context',
      query: 'Enterprise seat price',
      sourceIds: ['docs'],
    })
    assert.equal(act.success, true)
    assert.equal(act.outcome.reusedExistingContext, true)
  } finally {
    await s.close()
    try {
      rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
    } catch {}
  }
})

test('resolve verifies billing periods, persists conclusions, and reconciles Live changes', async () => {
  const stateless = new Scraper()
  try {
    const trap = await stateless.resolve({
      action: 'check',
      answer: 'Plan Pro costs $99/mo.',
      documents: [
        {
          url: 'https://vendor.example/pricing',
          markdown: '# Pricing\n\n| Plan | Annual Price |\n|---|---|\n| Plan Pro | $99 |\n',
        },
      ],
    })
    assert.equal(trap.success, true)
    assert.equal(trap.outcome.conclusion.state, 'refuted')
  } finally {
    await stateless.close()
  }

  const dir = mkdtempSync(join(tmpdir(), 'pagesift-node-res-'))
  const s = new Scraper({ storePath: dir })
  try {
    await s.context({
      action: 'ingest',
      sourceId: 'pricing',
      documents: [
        {
          url: 'https://vendor.example/pricing',
          markdown: '# Pricing\n\nPlan Pro costs $99/yr.\n',
        },
      ],
    })
    const checked = await s.resolve({
      action: 'check',
      sourceId: 'pricing',
      answer: 'Plan Pro costs $99/yr.',
      persist: true,
    })
    assert.equal(checked.success, true)
    assert.equal(checked.outcome.persisted, true)
    assert.equal(checked.outcome.conclusion.state, 'supported')
    const cid = checked.outcome.conclusion.conclusionId

    await s.context({
      action: 'ingest',
      sourceId: 'pricing',
      documents: [
        {
          url: 'https://vendor.example/pricing',
          markdown: '# Pricing\n\nPlan Pro costs $149/yr.\n',
        },
      ],
    })
    const rec = await s.resolve({ action: 'reconcile', sourceIds: ['pricing'] })
    assert.equal(rec.success, true)
    assert.ok(rec.report.impactedConclusionIds.includes(cid))

    const rev = await s.resolve({ action: 'revalidate', sourceIds: ['pricing'] })
    assert.equal(rev.success, true)
    assert.equal(rev.outcome.checkedCount, 1)
    assert.equal(rev.outcome.conclusions[0].state, 'refuted')
  } finally {
    await s.close()
    try {
      rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
    } catch {}
  }
})

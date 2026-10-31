// Actual native archive routes, scoped to a fresh fixture profile. Restart fixtures below rewrite
// only this test's own stopped receipts to exercise the saved publication boundary deterministically.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

if (!process.env.ZEUS_ARCHIVE_EVIDENCE) throw new Error('Set ZEUS_ARCHIVE_EVIDENCE to a new fixture directory')
const output = resolve(process.env.ZEUS_ARCHIVE_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a new scratch profile')
const reads = join(home, 'read scope'), writes = join(home, 'managed'), outside = join(home, 'outside')
for (const path of [reads, writes, outside]) mkdirSync(path, { recursive: true })
mkdirSync(join(reads, '資料'), { recursive: true })
writeFileSync(join(reads, '資料', '.hidden'), 'original bytes')
writeFileSync(join(outside, 'canary'), 'outside unchanged')
// Small stored ZIP fixtures: one corrupted payload with its original CRC, and two case-conflicting parent names.
// Kept here so a fresh acceptance run needs no manually prepared external files.
writeFileSync(join(reads, 'bad-crc.zip'), Buffer.from('UEsDBBQAAAAAAIcRX11ncVQjGQAAABkAAAALAAAAZml4dHVyZS50eHRgcmNoaXZlIENSQyBmaXh0dXJlIGJ5dGVzUEsBAhQDFAAAAAAAhxFfXWdxVCMZAAAAGQAAAAsAAAAAAAAAAAAAAIABAAAAAGZpeHR1cmUudHh0UEsFBgAAAAABAAEAOQAAAEIAAAAAAA==', 'base64'))
writeFileSync(join(reads, 'parent-case.zip'), Buffer.from('UEsDBBQAAAAAAIcRX11DvrfoAQAAAAEAAAAMAAAAUGFyZW50L2EudHh0YVBLAwQUAAAAAACHEV9d+e++cQEAAAABAAAADAAAAHBhcmVudC9iLnR4dGJQSwECFAMUAAAAAACHEV9dQ7636AEAAAABAAAADAAAAAAAAAAAAAAAgAEAAAAAUGFyZW50L2EudHh0UEsBAhQDFAAAAAAAhxFfXfnvvnEBAAAAAQAAAAwAAAAAAAAAAAAAAIABKwAAAHBhcmVudC9iLnR4dFBLBQYAAAAAAgACAHQAAABWAAAAAAA=', 'base64'))
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
const receiptPath = id => join(home, 'app', 'index', 'archives', `${id}.json`)
const receipt = id => JSON.parse(readFileSync(receiptPath(id), 'utf8'))
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  const config = await h.get('/native/v1/index/config')
  config.roots.push(...[reads, writes].map(path => ({ path, include: true, monitor_changes: false, rescan_every_hours: null, download: false, manage_files: path === writes })))
  await h.put('/native/v1/index/config', config)
  const refuse = async (name, action, body) => {
    let error
    try { await h.post(`/native/v1/files/archive/${action}`, body) } catch (e) { error = e }
    check(name, !!error && error.status >= 400 && !!error.code)
  }
  const plan = (kind, source, name, directory = writes) => h.post('/native/v1/files/archive/prepare', { kind, source, directory, name })
  const getJob = async id => (await h.get('/native/v1/files/archives')).jobs.find(j => j.id === id)
  const run = async id => {
    await h.post('/native/v1/files/archive/start', { id })
    return until(async () => { const job = await getJob(id); return !['ready', 'running', 'publishing'].includes(job?.state) && job }, { what: 'native archive completion' })
  }
  await refuse('a read scope cannot receive archive output', 'prepare', { kind: 'create', source: join(reads, '資料'), directory: reads, name: 'forbidden.zip' })
  await refuse('implicit case-conflicting parent folders fail preview', 'prepare', { kind: 'extract', source: join(reads, 'parent-case.zip'), directory: writes, name: 'bad-case' })
  const linkedSource = join(reads, '資料', 'junction'), linkedDest = join(writes, 'junction')
  symlinkSync(outside, linkedSource, process.platform === 'win32' ? 'junction' : 'dir')
  await refuse('source-tree junctions fail before staging', 'prepare', { kind: 'create', source: join(reads, '資料'), directory: writes, name: 'bad-source.zip' })
  rmSync(linkedSource)
  symlinkSync(outside, linkedDest, process.platform === 'win32' ? 'junction' : 'dir')
  await refuse('destination junctions cannot redirect publication', 'prepare', { kind: 'create', source: join(reads, '資料'), directory: linkedDest, name: 'escape.zip' })
  rmSync(linkedDest)
  const destination = join(writes, 'reviewed folder')
  mkdirSync(destination)
  const replaced = await plan('create', join(reads, '資料'), 'replacement.zip', destination)
  renameSync(destination, join(writes, 'old reviewed folder')); mkdirSync(destination)
  const refused = await run(replaced.id)
  check('replacement destination identity invalidates a saved preview', refused.state === 'failed' && !refused.staging && !existsSync(join(destination, 'replacement.zip')))
  const collision = await plan('create', join(reads, '資料'), 'occupied.zip')
  writeFileSync(join(writes, 'occupied.zip'), 'foreign output')
  check('start refuses an output created after preview without overwriting it', (await run(collision.id)).state === 'failed' && readFileSync(join(writes, 'occupied.zip'), 'utf8') === 'foreign output')
  const create = await plan('create', join(reads, '資料'), 'bundle.zip')
  check('preview preserves Unicode and hidden member names', create.entries.some(e => e.name === '資料/.hidden'))
  const created = await run(create.id)
  check('ZIP creation publishes a SHA-256 receipt', created.state === 'complete' && created.sha256.length === 64 && !created.staging)
  const extract = await plan('extract', join(writes, 'bundle.zip'), 'unpacked')
  const extracted = await run(extract.id)
  check('extraction preserves Unicode names and exact hidden-file bytes', extracted.state === 'complete' && readFileSync(join(writes, 'unpacked', '資料', '.hidden'), 'utf8') === 'original bytes')
  await refuse('completed output cannot be overwritten by a new preview', 'prepare', { kind: 'extract', source: join(writes, 'bundle.zip'), directory: writes, name: 'unpacked' })
  const cleanupPlan = await plan('extract', join(reads, 'bad-crc.zip'), 'crc-output')
  const replacementPlan = await plan('extract', join(reads, 'bad-crc.zip'), 'crc-replacement-output')
  for (const p of [cleanupPlan, replacementPlan]) {
    const failed = await run(p.id)
    check(`CRC failure retains recorded private staging (${p.id === cleanupPlan.id ? 'cleanup' : 'replacement'})`, failed.state === 'failed' && !!failed.staging && !existsSync(p.target))
  }
  const legacy = await plan('create', join(reads, '資料'), 'legacy.zip')
  await h.stop(); h = undefined
  const cleanStage = receipt(cleanupPlan.id).staging, replacementStage = receipt(replacementPlan.id).staging
  symlinkSync(outside, join(cleanStage, 'junction'), process.platform === 'win32' ? 'junction' : 'dir')
  renameSync(replacementStage, join(writes, 'original-failed-stage')); mkdirSync(replacementStage)
  writeFileSync(join(replacementStage, 'foreign'), 'foreign staging')
  const recovery = receipt(extract.id)
  recovery.state = 'publishing'
  recovery.staging = join(writes, `.zeus-archive-${extract.id}-${'d'.repeat(32)}`)
  writeFileSync(receiptPath(extract.id), JSON.stringify(recovery))
  const oldPlan = receipt(legacy.id); delete oldPlan.plan.target_parent_id
  writeFileSync(receiptPath(legacy.id), JSON.stringify(oldPlan))
  h = await startHarness({ home }); await h.ready()
  check('publication-boundary receipt restarts as interrupted', (await getJob(extract.id)).state === 'interrupted')
  await refuse('cleanup rejects a replacement staging object', 'cleanup', { id: replacementPlan.id })
  check('foreign replacement contents remain untouched', readFileSync(join(replacementStage, 'foreign'), 'utf8') === 'foreign staging')
  await refuse('cleanup preflight rejects linked descendants', 'cleanup', { id: cleanupPlan.id })
  check('failed preflight preserves recorded staging for review', existsSync(cleanStage) && !!(await getJob(cleanupPlan.id)).staging)
  rmSync(join(cleanStage, 'junction'))
  await h.post('/native/v1/files/archive/cleanup', { id: cleanupPlan.id })
  check('owned link-free staging cleans up after restart', !existsSync(cleanStage) && !(await getJob(cleanupPlan.id)).staging)
  const published = join(writes, 'unpacked', '資料', '.hidden')
  writeFileSync(published, 'modified bytes')
  await refuse('reconciliation refuses changed published bytes', 'reconcile', { id: extract.id })
  check('digest failure never asserts completion', (await getJob(extract.id)).state === 'interrupted')
  writeFileSync(published, 'original bytes')
  await h.post('/native/v1/files/archive/reconcile', { id: extract.id })
  check('explicit reconciliation accepts the original identity and digest', (await getJob(extract.id)).state === 'complete')
  check('legacy ready receipts require a fresh preview', (await run(legacy.id)).state === 'failed' && !existsSync(join(writes, 'legacy.zip')))
  check('source and ungranted outside contents remain untouched', readFileSync(join(reads, '資料', '.hidden'), 'utf8') === 'original bytes' && readFileSync(join(outside, 'canary'), 'utf8') === 'outside unchanged' && !existsSync(join(outside, 'escape.zip')))
  ui = await openUi(h)
  await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
  await ui.page.locator('.archive-job').first().waitFor()
  check('real UI shows native complete and unresolved archive receipts', await ui.page.locator('.archive-job').filter({ hasText: 'Extract ZIP · complete' }).count() > 0 && await ui.page.getByRole('button', { name: 'Clean private staging', exact: true }).count() > 0)
  await ui.page.screenshot({ path: join(output, 'archive-recovery.png'), fullPage: true })
  check('archive receipt UI has no page errors', ui.errors.length === 0)
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, platform: process.platform, receiptBoundaryFixture: true, pageErrors: ui.errors }, null, 2))
} catch (error) {
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), checks, body: ui ? await ui.page.locator('body').innerText() : null }, null, 2))
  throw error
} finally { await ui?.close(); await h?.stop() }

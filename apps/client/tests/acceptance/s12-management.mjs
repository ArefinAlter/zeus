// 0.0.1c: direct folder browsing and background copies against the real desktop engine.
// ZEUS_CLIENT_TARGET, ZEUS_BIN_DIR and ZEUS_UI_DIR select the current validated builds.
import assert from 'node:assert/strict'
import { mkdirSync, writeFileSync, readFileSync, existsSync, symlinkSync } from 'node:fs'
import { join } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const output = process.env.ZEUS_MANAGEMENT_EVIDENCE ?? 'D:/verne-build/0.0.1c/evidence/full-c/files'
const home = join(output, 'profile')
mkdirSync(output, { recursive: true })
const h = await startHarness({ home, fresh: true })
const checks = []
let ui
const check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
const finished = async id => until(async () => {
  const job = await h.post('/native/v1/files/copy/status', { id })
  return ['complete', 'failed', 'cancelled'].includes(job.state) && job
}, { timeoutMs: 60000, everyMs: 50, what: 'copy completion' })
try {
  await h.ready()
  const read = join(home, 'search-only')
  const managed = join(home, 'managed')
  const outside = join(home, 'not-granted')
  for (const path of [read, managed, outside]) mkdirSync(path, { recursive: true })
  const config = await h.get('/native/v1/index/config')
  config.roots.push(...[read, managed].map(path => ({ path, include: true, monitor_changes: false, rescan_every_hours: null, download: false, manage_files: path === managed })))
  config.exclusions.push('excluded-from-search')
  await h.put('/native/v1/index/config', config)
  const folder = join(read, 'excluded-from-search')
  mkdirSync(join(folder, 'nested'), { recursive: true })
  writeFileSync(join(folder, 'nested', '.hidden.txt'), 'retained fixture content')
  for (let i = 0; i < 1005; i++) writeFileSync(join(folder, `${String(i).padStart(4, '0')}.txt`), `fixture ${i}`)
  let cursor = '', files = []
  do {
    const page = await h.post('/native/v1/files/list', { path: folder, after: cursor, limit: 500 })
    assert.ok(page.files.length <= 500)
    files.push(...page.files)
    cursor = page.next ?? ''
  } while (cursor)
  check('live folder pages include all 1006 entries excluded from search', files.length === 1006 && new Set(files.map(f => f.path)).size === 1006)
  const job = await h.post('/native/v1/files/copy/start', { path: folder, dir: managed })
  const copied = await finished(job.id)
  check('folder copy completes with exact nested hidden-file bytes', copied.state === 'complete' && readFileSync(join(managed, 'excluded-from-search', 'nested', '.hidden.txt'), 'utf8') === 'retained fixture content')
  check('copy preserves source and counts all regular files', existsSync(join(folder, 'nested', '.hidden.txt')) && copied.files === 1006)
  const collision = await finished((await h.post('/native/v1/files/copy/start', { path: folder, dir: managed })).id)
  check('copy refuses an existing destination', collision.state === 'failed' && readFileSync(join(managed, 'excluded-from-search', '0000.txt'), 'utf8') === 'fixture 0')
  const refused = await finished((await h.post('/native/v1/files/copy/start', { path: folder, dir: outside })).id)
  check('copy cannot write outside management grants', refused.state === 'failed' && !existsSync(join(outside, 'excluded-from-search')))
  let readOnly = false
  try { await h.post('/native/v1/files/mkdir', { parent: read, name: 'must-not-create' }) } catch { readOnly = true }
  check('search grants do not permit writes', readOnly && !existsSync(join(read, 'must-not-create')))
  await h.post('/native/v1/files/mkdir', { parent: managed, name: 'new-folder' })
  check('folder creation uses the explicit management grant', existsSync(join(managed, 'new-folder')))
  const large = join(read, 'cancel-copy.bin')
  writeFileSync(large, Buffer.alloc(128 * 1024 * 1024, 0x5a))
  const cancelJob = await h.post('/native/v1/files/copy/start', { path: large, dir: managed })
  await h.post('/native/v1/files/copy/cancel', { id: cancelJob.id })
  const cancelled = await finished(cancelJob.id)
  check('running copies can be cancelled and keep the source', cancelled.state === 'cancelled' && existsSync(large) && !existsSync(join(managed, 'cancel-copy.bin')))
  const link = join(read, 'share-link')
  try {
    symlinkSync(outside, link, process.platform === 'win32' ? 'junction' : 'dir')
    const listing = await h.post('/native/v1/files/list', { path: read })
    check('live browsing omits links and junctions', !listing.files.some(file => file.path === link))
    const rejected = await finished((await h.post('/native/v1/files/copy/start', { path: link, dir: managed })).id)
    check('copy refuses links and junctions without output', rejected.state === 'failed' && !existsSync(join(managed, 'share-link')))
  } catch (error) {
    if (['EPERM', 'EACCES'].includes(error.code)) console.log('SKIP link fixture: OS permission')
    else throw error
  }
  ui = await openUi(h)
  await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files?path=${encodeURIComponent(folder)}`)
  await ui.page.getByText('500 items loaded', { exact: true }).waitFor()
  await ui.page.getByRole('button', { name: 'Load more', exact: true }).click()
  await ui.page.getByText('1,000 items loaded', { exact: true }).waitFor()
  await ui.page.getByRole('button', { name: 'Load more', exact: true }).click()
  await ui.page.getByText('1,006 items loaded', { exact: true }).waitFor()
  check('the real file-manager UI loads every live folder page', await ui.page.getByRole('button', { name: 'Load more', exact: true }).count() === 0)
  check('file-manager UI has no page errors', ui.errors.length === 0)
  await ui.page.screenshot({ path: join(output, 'live-folder.png'), fullPage: true })
  writeFileSync(join(output, 'results.json'), JSON.stringify({ passed: true, checks, pageErrors: ui.errors, backend: 'real zeus-app-http and zeusd, isolated profile' }, null, 2))
} finally {
  await ui?.close()
  await h.stop()
}

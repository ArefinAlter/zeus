// Real native index and UI, isolated files/profile. No human approval or owner-profile access.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, writeFileSync, readFileSync, linkSync, unlinkSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'
const output = process.env.ZEUS_STORAGE_EVIDENCE
if (!output) throw Error('Set ZEUS_STORAGE_EVIDENCE to a fresh private directory')
const home = resolve(join(output, 'profile'))
if (existsSync(home)) throw Error('Use a fresh profile')
mkdirSync(home, { recursive: true })
const scope = join(home, 'read scope'), outside = join(home, 'outside')
mkdirSync(scope); mkdirSync(outside)
const bytes = Buffer.alloc(8192, 42), source = join(scope, 'file.bin'), alias = join(scope, 'alias.bin')
writeFileSync(source, bytes); linkSync(source, alias); writeFileSync(join(outside, 'excluded.bin'), Buffer.alloc(16384))
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  const config = await h.get('/native/v1/index/config')
  config.roots.push({ path: scope, include: true, monitor_changes: true, rescan_every_hours: null, download: false, manage_files: false })
  await h.put('/native/v1/index/config', config); await h.post('/svc/v1/index/rescan')
  const first = await until(async () => {
    const value = await h.get('/native/v1/index/analytics')
    return value.files === 2 && value.storage?.known_objects === 1 && value
  }, { what: 'native alias measurements' })
  check('two names have one measured file object', first.storage.observed_aliases === 1 && first.logical_bytes === 16384 && first.storage.unique_logical_bytes_known === 8192)
  check('native filesystem reports allocation separately from logical length', first.storage.complete && Number.isSafeInteger(first.storage.allocated_bytes_known) && first.storage.allocated_bytes_known >= 0)
  check('hard-link aliases are not duplicate-copy candidates', first.duplicate_candidates.length === 0)
  check('ungranted outside files never enter the report', !JSON.stringify(first).includes(outside) && first.files === 2)
  check('accounting makes no reclaimable-space promise', first.storage.reclaimable_bytes === null && first.storage.shared_extents_deduplicated === false)
  ui = await openUi(h)
  await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
  await ui.page.getByRole('button', { name: 'Storage and history', exact: true }).click()
  const panel = ui.page.getByLabel('Observed storage allocation', { exact: true })
  await panel.waitFor()
  check('real Storage UI distinguishes known objects and hard-link names', (await panel.innerText()).includes('1 known file objects') && (await panel.innerText()).includes('1 additional hard-link names'))
  for (const width of [1440, 960, 390]) {
    await ui.page.setViewportSize({ width, height: 850 }); await ui.page.waitForTimeout(100)
    check('storage dialog stays inside viewport at ' + width, await ui.page.getByRole('dialog').evaluate(node => {
      const bounds = node.getBoundingClientRect()
      return bounds.left >= -1 && bounds.right <= innerWidth + 1 && node.scrollWidth <= node.clientWidth + 1
    }))
    await ui.page.screenshot({ path: join(output, `storage-${width}.png`) })
  }
  check('no frontend errors', ui.errors.length === 0)
  await ui.close(); ui = null; await h.stop()
  h = await startHarness({ home }); await h.ready()
  const after = await until(async () => { const value = await h.get('/native/v1/index/analytics'); return value.storage?.observed_aliases === 1 && value })
  check('storage facts survive cache persistence and restart', after.storage.allocated_bytes_known === first.storage.allocated_bytes_known && after.storage.known_objects === 1)
  unlinkSync(alias); await h.post('/svc/v1/index/rescan')
  await until(async () => { const value = await h.get('/native/v1/index/analytics'); return value.files === 1 && value.storage.observed_aliases === 0 && value.storage.unobserved_aliases === 0 })
  check('rescan reconciles removed alias and current hard-link count', true)
  config.roots = config.roots.filter(root => root.path !== scope)
  await h.put('/native/v1/index/config', config)
  const revoked = await h.get('/native/v1/index/analytics')
  check('revoked root measurements disappear immediately', revoked.files === 0 && revoked.storage.known_objects === 0)
  check('source bytes remain unchanged', readFileSync(source).equals(bytes))
  check('no approvals were created', (await h.get('/api/v1/approvals')).items.length === 0)
  writeFileSync(join(output, 'result.json'), JSON.stringify({ ok: true, checks, first, after, approvals_made: 0 }, null, 2))
} catch (error) {
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), checks, body: await ui?.page.locator('body').innerText().catch(() => ''), stderr: h?.stderr() }, null, 2))
  await ui?.page.screenshot({ path: join(output, 'failure.png') }).catch(() => {})
  throw error
} finally { await ui?.close(); await h?.stop() }

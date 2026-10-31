// Real native maintenance preview/apply/status and responsive UI in an isolated profile.
// Human approvals remain pending; this test never impersonates a person or grants approval.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

if (!process.env.ZEUS_MAINTENANCE_EVIDENCE) throw new Error('Set ZEUS_MAINTENANCE_EVIDENCE to a new evidence directory')
const output = resolve(process.env.ZEUS_MAINTENANCE_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a fresh profile')
mkdirSync(output, { recursive: true })
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  const artifact = (op, body = {}) => h.post(`/native/v1/artifacts/${op}`, body)
  const refused = async (name, fn) => { let denied = false; try { await fn() } catch (e) { denied = e.status >= 400 && !!e.code } check(name, denied) }
  const root = (await h.post('/api/v1/session')).roots[0]
  const input = join(root, 'maintenance.txt'), bundle = join(root, 'metadata.zeusab')
  writeFileSync(input, 'Owned maintenance fixture')
  const { artifact: record } = await artifact('ingest', { path: input, idempotency_key: 'maintenance-register' })
  await refused('maintenance route cannot decide approvals', () => artifact('maintenance-approve', { decision: 'approve' }))
  await refused('maintenance route cannot dispatch arbitrary RPC methods', () => artifact('maintenance-preview', { method: 'approvals.decide', operation: { kind: 'import', path: input } }))
  ui = await openUi(h); const page = ui.page
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/library`)
  await page.getByLabel('Maintenance artifact IDs').fill(record.id)
  await page.getByLabel('Maintenance file path').fill(bundle)
  await page.getByRole('button', { name: 'Preview maintenance', exact: true }).click()
  await page.getByLabel('Exact maintenance review', { exact: true }).waitFor()
  check('desktop shows exact bundle manifest and content identity before effect', (await page.getByLabel('Exact maintenance plan').innerText()).includes('manifest_blake3') && !existsSync(bundle))
  const download = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Save maintenance request', exact: true }).click()
  const saved = await download, savedPath = join(output, 'export-request.json'); await saved.saveAs(savedPath)
  const request = JSON.parse(readFileSync(savedPath, 'utf8'))
  check('saved request retains exact plan hash and idempotency key', /^[0-9a-f]{64}$/.test(request.plan_hash) && request.idempotency_key.startsWith('maintenance-'))
  await refused('status before applying does not invent a completed request', () => artifact('maintenance-status', request))
  await page.getByRole('button', { name: 'Apply reviewed maintenance', exact: true }).click()
  await until(async () => (await page.getByLabel('Maintenance receipt').innerText()).includes('Status: completed'))
  check('metadata export creates a validated bounded bundle without moving the user file', readFileSync(bundle).subarray(0, 8).toString() === 'ZEUSAB1\n' && readFileSync(input, 'utf8') === 'Owned maintenance fixture')
  const exported = readFileSync(bundle)
  writeFileSync(bundle, 'Later user edit')
  const replay = await artifact('maintenance-apply', request), status = await artifact('maintenance-status', request)
  check('completed retry and read-only status preserve later edits', replay.applied && replay.replayed && status.applied && readFileSync(bundle, 'utf8') === 'Later user edit')
  // Self-import is deliberately rejected. Use a second real store for cross-workspace import.
  const selfImport = join(root, 'self-import.zeusab'); writeFileSync(selfImport, exported)
  const selfPlan = await artifact('maintenance-preview', { operation: { kind: 'import', path: selfImport } })
  const rejected = await artifact('maintenance-apply', { ...selfPlan, idempotency_key: 'self-import' })
  check('self-import refuses conflicting source identity without adding records', !rejected.applied && rejected.error_code === 'artifact.import_conflict' && (await artifact('list', { limit: 50 })).items.length === 1)
  await ui.close(); ui = null; await h.stop()
  h = await startHarness({ home: join(output, 'import-profile') }); await h.ready()
  const importRoot = (await h.post('/api/v1/session')).roots[0]
  const importPath = join(importRoot, 'import.zeusab'); writeFileSync(importPath, exported)
  const importing = await artifact('maintenance-preview', { operation: { kind: 'import', path: importPath } })
  const imported = await artifact('maintenance-apply', { ...importing, idempotency_key: 'maintenance-import' })
  check('metadata import resolves immutable source evidence in a different store', imported.applied && imported.artifact_ids.length === 1)
  const repairId = imported.artifact_ids[0], received = (await artifact('get', { artifact_id: repairId })).artifact
  check('metadata import preserves both digests without inventing stored bytes', received.sha256 === record.sha256 && received.blake3 === record.blake3 && received.object.state === 'absent')
  const repairInput = join(importRoot, 'repair-source.txt'); writeFileSync(repairInput, readFileSync(input))
  ui = await openUi(h)
  const importPage = ui.page
  await importPage.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/library`)
  await importPage.getByLabel('Maintenance operation').selectOption('repair')
  await importPage.getByLabel('Maintenance artifact IDs').fill(repairId)
  await importPage.getByLabel('Maintenance file path').fill(repairInput)
  await importPage.getByRole('button', { name: 'Preview maintenance', exact: true }).click()
  await importPage.getByRole('button', { name: 'Request maintenance approval', exact: true }).click()
  await importPage.getByRole('button', { name: 'Review maintenance in human terminal', exact: true }).waitFor()
  check('repair waits for a separate human decision', (await importPage.getByLabel('Maintenance receipt').innerText()).includes('awaiting approval') && (await artifact('get', { artifact_id: repairId })).artifact.object.state === 'absent')
  const repairDownload = importPage.waitForEvent('download'); await importPage.getByRole('button', { name: 'Save maintenance request', exact: true }).click()
  const repairPath = join(output, 'repair-request.json'); await (await repairDownload).saveAs(repairPath)
  const repairRequest = JSON.parse(readFileSync(repairPath, 'utf8'))
  await importPage.reload(); await importPage.getByLabel('Restore maintenance request').setInputFiles(repairPath)
  await until(async () => (await importPage.getByLabel('Maintenance receipt').innerText()).includes('awaiting approval'))
  check('restoring a saved review reads status without applying or approving it', (await artifact('get', { artifact_id: repairId })).artifact.object.state === 'absent' && !(await artifact('maintenance-status', repairRequest)).applied)
  await importPage.getByText('Artifact event replay', { exact: true }).click()
  await importPage.getByRole('button', { name: 'Read artifact events', exact: true }).click()
  await until(async () => (await importPage.getByLabel('Artifact events').innerText()).includes('artifact.maintenance_completed'))
  check('event replay distinguishes recorded completion', (await importPage.getByLabel('Artifact events').innerText()).includes('"phase": "completed"'))
  for (const [width, height] of [[1440, 900], [960, 600], [390, 740]]) {
    await importPage.setViewportSize({ width, height }); await importPage.waitForTimeout(150)
    const dims = await importPage.evaluate(() => ({ sw: document.documentElement.scrollWidth, sh: document.documentElement.scrollHeight }))
    check(`maintenance page remains contained at ${width}x${height}`, dims.sw <= width + 1 && dims.sh <= height + 1)
    const overflow = await importPage.locator('[aria-label="Artifact maintenance"] input, [aria-label="Artifact maintenance"] select, [aria-label="Artifact maintenance"] textarea, [aria-label="Artifact maintenance"] button').evaluateAll(nodes => nodes.filter(node => {
      const box = node.getBoundingClientRect(), card = node.closest('.runs-card').getBoundingClientRect()
      return box.width > 0 && (box.left < card.left - 1 || box.right > card.right + 1)
    }).map(node => node.getAttribute('aria-label') || node.textContent || node.tagName))
    check(`maintenance controls stay inside their cards at ${width}x${height}: ${overflow.join(', ')}`, overflow.length === 0)
    await importPage.screenshot({ path: join(output, `maintenance-${width}.png`) })
  }
  const restartHome = h.home
  await ui.close(); ui = null; await h.stop(); h = await startHarness({ home: restartHome }); await h.ready()
  const pending = await h.post('/native/v1/artifacts/maintenance-status', repairRequest)
  check('pending maintenance receipts survive restart without effects', pending.status === 'awaiting_approval' && !pending.applied && readFileSync(input, 'utf8') === 'Owned maintenance fixture')
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, approvals_made: 0, platform: process.platform }, null, 2))
} catch (error) {
  const page = ui?.page
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), checks, text: await page?.locator('body').innerText().catch(() => ''), engine_stderr: h?.stderr() }, null, 2))
  await page?.screenshot({ path: join(output, 'failure.png') }).catch(() => {})
  throw error
} finally { await ui?.close(); await h?.stop() }

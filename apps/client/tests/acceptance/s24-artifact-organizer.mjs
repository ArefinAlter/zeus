// Real native artifact/organization requests in a new profile. No approval is granted.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

if (!process.env.ZEUS_LIBRARY_EVIDENCE) throw new Error('Set ZEUS_LIBRARY_EVIDENCE to a new evidence directory')
const output = resolve(process.env.ZEUS_LIBRARY_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a fresh profile')
mkdirSync(output, { recursive: true })
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  const artifact = (op, body = {}) => h.post(`/native/v1/artifacts/${op}`, body)
  const organize = (op, body) => h.post(`/native/v1/organize/${op}`, body)
  const refused = async (name, fn) => { let denied = false; try { await fn() } catch (e) { denied = e.status >= 400 && !!e.code } check(name, denied) }
  const root = (await h.post('/api/v1/session')).roots[0]
  const input = join(root, 'alpha.pdf'), other = join(root, 'second.txt'), outside = join(home, 'outside.txt')
  writeFileSync(input, '%PDF-1.4\nOwned artifact fixture\n'); writeFileSync(other, 'Second owned fixture'); writeFileSync(outside, 'Outside daemon write roots')
  check('new native artifact library starts empty', (await artifact('list', { limit: 50 })).items.length === 0)
  await refused('artifact ingestion cannot leave configured write roots', () => artifact('ingest', { path: outside, idempotency_key: 'outside-ingest' }))
  await refused('library route cannot decide approvals', () => artifact('approvals.decide', { decision: 'approve' }))
  await refused('library route cannot grant write roots', () => artifact('files.roots.register', { path: home }))
  ui = await openUi(h); const page = ui.page
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/library`)
  await page.getByLabel('Artifact ingest path').fill(input)
  await page.getByRole('button', { name: 'Register file', exact: true }).click()
  await page.getByRole('region', { name: 'Artifact details', exact: true }).waitFor()
  const record = (await artifact('list', { limit: 50 })).items[0]
  check('desktop registration hashes the actual file without moving it', record.sha256 === createHash('sha256').update(readFileSync(input)).digest('hex') && existsSync(input))
  const params = { path: other, idempotency_key: 'library-register-retry' }
  const first = await artifact('ingest', params), second = await artifact('ingest', params)
  check('artifact ingestion retries resolve to the same record', first.artifact.id === second.artifact.id && second.replayed)
  const firstPage = await artifact('list', { limit: 1 })
  const nextPage = await artifact('list', { limit: 1, cursor: firstPage.next_cursor })
  check('native artifact list pages without repeating the first record', firstPage.next_cursor && nextPage.items.length === 1 && firstPage.items[0].id !== nextPage.items[0].id)
  await page.getByRole('button', { name: 'Show ancestors', exact: true }).click()
  await page.getByLabel('Artifact lineage').waitFor()
  check('native provenance traversal reports the starting record', (await page.getByLabel('Artifact lineage').innerText()).includes(record.id))
  await page.getByLabel('Artifact lifecycle action').selectOption('keep')
  await page.getByRole('button', { name: 'Preview artifact change', exact: true }).click()
  await page.getByRole('button', { name: 'Apply reviewed artifact change', exact: true }).waitFor()
  await page.getByRole('button', { name: 'Apply reviewed artifact change', exact: true }).click()
  await page.getByText('The reviewed change completed.', { exact: true }).waitFor()
  check('non-destructive retention strengthening uses native lifecycle API', (await artifact('get', { artifact_id: record.id })).artifact.retention === 'keep')
  const deletion = await artifact('preview', { artifact_id: record.id, operation: { kind: 'delete' } })
  await refused('lifecycle apply refuses a stale preview hash', () => artifact('apply', { ...deletion, plan_hash: '0'.repeat(64), idempotency_key: 'invalid-lifecycle-hash' }))
  const deletionParams = { ...deletion, idempotency_key: 'pending-library-delete' }
  const pending = await artifact('apply', deletionParams), pendingRetry = await artifact('apply', deletionParams)
  check('record tombstoning stays pending until human approval', !pending.applied && pending.approval_id && (await artifact('get', { artifact_id: record.id })).artifact.state === 'active' && existsSync(input))
  check('pending artifact effects keep their retry receipt', pending.approval_id === pendingRetry.approval_id && pendingRetry.replayed)
  const rules = { recursive: false, rules: [{ when: { ext: ['pdf'] }, subdir: 'Organized' }] }
  const forcedPreview = await organize('plan', { dir: root, rules, dry_run: false, idempotency_key: 'forced-preview' })
  check('organizer preview cannot be switched into mutation by the caller', forcedPreview.steps.length === 1 && !forcedPreview.applied && !forcedPreview.approval_id && existsSync(input))
  await refused('organizer refuses natural-language authority input', () => organize('plan', { dir: root, rules: 'delete everything' }))
  await page.getByLabel('Organization folder').fill(root)
  await page.getByLabel('Organization rules JSON').fill(JSON.stringify(rules, null, 2))
  await page.getByRole('button', { name: 'Preview organization', exact: true }).click()
  await until(async () => await page.getByRole('button', { name: 'Request organization approval', exact: true }).isEnabled())
  check('desktop organization review displays native before/after steps', (await page.getByLabel('Organization preview').innerText()).includes('alpha.pdf'))
  await page.getByLabel('Organization rules JSON').fill(JSON.stringify({ ...rules, recursive: true }, null, 2))
  check('editing organization rules invalidates the previous review', await page.getByRole('button', { name: 'Request organization approval', exact: true }).isDisabled())
  await page.getByLabel('Organization rules JSON').fill(JSON.stringify(rules, null, 2))
  await page.getByRole('button', { name: 'Preview organization', exact: true }).click()
  await until(async () => await page.getByRole('button', { name: 'Request organization approval', exact: true }).isEnabled())
  await page.getByRole('button', { name: 'Request organization approval', exact: true }).click()
  await page.getByRole('button', { name: 'Review organization approval', exact: true }).waitFor()
  const organizationId = await page.getByLabel('Organization undo operation').inputValue()
  const status = await organize('get', { operation_id: organizationId })
  check('native organizer reports durable pending status and exact counts', status.state === 'awaiting_approval' && status.pending === 1 && status.completed === 0)
  check('organization request cannot move files before a human approves', existsSync(input) && !existsSync(join(root, 'Organized', 'alpha.pdf')))
  for (const [width, height] of [[1440, 900], [960, 600], [390, 740]]) {
    await page.setViewportSize({ width, height }); await page.waitForTimeout(150)
    const dims = await page.evaluate(() => ({ sw: document.documentElement.scrollWidth, sh: document.documentElement.scrollHeight }))
    check(`library panels remain contained at ${width}x${height}`, dims.sw <= width + 1 && dims.sh <= height + 1)
    const overflow = await page.locator('.runs-card input, .runs-card select, .runs-card textarea, .runs-card button').evaluateAll(nodes => nodes.filter(node => {
      const box = node.getBoundingClientRect(), card = node.closest('.runs-card').getBoundingClientRect()
      return box.width > 0 && (box.left < card.left - 1 || box.right > card.right + 1)
    }).map(node => node.getAttribute('aria-label') || node.textContent || node.tagName))
    check(`form controls stay inside their cards at ${width}x${height}: ${overflow.join(', ')}`, overflow.length === 0)
    await page.screenshot({ path: join(output, `library-${width}.png`) })
  }
  await ui.close(); ui = null; await h.stop()
  h = await startHarness({ home }); await h.ready()
  check('artifact records and pending effects survive daemon restart without execution', (await h.post('/native/v1/artifacts/get', { artifact_id: record.id })).artifact.state === 'active' && existsSync(input) && !existsSync(join(root, 'Organized', 'alpha.pdf')))
  check('organizer status survives daemon restart', (await h.post('/native/v1/organize/get', { operation_id: organizationId })).state === 'awaiting_approval')
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, approvals_made: 0, platform: process.platform }, null, 2))
} catch (error) {
  const page = ui?.page
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), checks, text: await page?.locator('body').innerText().catch(() => ''), engine_stderr: h?.stderr() }, null, 2))
  await page?.screenshot({ path: join(output, 'failure.png') }).catch(() => {})
  console.error(String(error)); throw error
} finally { await ui?.close(); await h?.stop() }

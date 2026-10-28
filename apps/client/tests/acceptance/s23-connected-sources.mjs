// Native daemon and UI boundary acceptance, using an empty, fresh profile.
// No source grants, credentials, account configuration or live transfers are created.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness } from '../harness.mjs'
import { openUi } from '../ui.mjs'

if (!process.env.ZEUS_SOURCES_EVIDENCE) throw new Error('Set ZEUS_SOURCES_EVIDENCE to a fresh scratch directory')
const output = resolve(process.env.ZEUS_SOURCES_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a fresh scratch profile')
mkdirSync(output, { recursive: true })
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  const request = (op, body = {}) => h.post(`/native/v1/sources/${op}`, body)
  const refused = async (name, fn) => {
    let denied = false
    try { await fn() } catch (e) { denied = e.status >= 400 && !!e.code }
    check(name, denied)
  }
  check('fresh native profile has no implicit source grants', (await request('accounts')).accounts.length === 0)
  check('native source operations start empty', (await request('list')).operations.length === 0)
  await refused('webview cannot configure a source account', () => request('accounts.configure', {}))
  await refused('webview cannot revoke account authority directly', () => request('accounts.revoke', {}))
  await refused('source terminal action is closed to arbitrary commands', () => request('review-terminal', { action: 'run', id: 'echo unsafe' }))
  await refused('source revocation terminal refuses injected account IDs', () => request('review-terminal', { action: 'revoke', id: 'account;echo unsafe' }))
  await refused('inspection needs an existing grant for this requester', () => request('inspect', {
    account_id: 'missing-account', account_revision: 1,
    target: { provider: 'hugging_face', repository_type: 'model', repository: 'fixture/model', commit: 'a'.repeat(40), path: 'weights.bin' },
    budget: { max_received_bytes: 1024, timeout_ms: 1000 }, metainfo_base64: null, idempotency_key: 'missing-source-inspect',
  }))
  await refused('source dispatch needs an existing reviewed plan', () => request('submit', {
    plan_id: 'missing-plan', dest_dir: null, filename: 'fixture.bin', max_connections: 1,
    torrent_filenames: {}, idempotency_key: 'missing-source-submit',
  }))
  await refused('source control cannot name an unknown operation', () => request('control', {
    operation_id: 'missing-operation', action: 'cancel', idempotency_key: 'missing-source-cancel',
  }))
  check('refused source requests create no download jobs', (await h.get('/api/v1/jobs')).items.length === 0)
  ui = await openUi(h)
  const page = ui.page
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/sources`)
  await page.getByText('No source is available to this desktop yet. During setup, explicitly allow this installation’s app client.', { exact: true }).waitFor()
  check('source review is disabled until explicit account setup', await page.getByRole('button', { name: 'Review source', exact: true }).isDisabled())
  await page.route('**/native/v1/sources/accounts', route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { code: 'fixture.offline', message: 'Injected source account read failure' } }) }))
  await page.getByRole('button', { name: 'Refresh sources', exact: true }).click()
  await page.getByText(/Injected source account read failure/).waitFor()
  check('account read failure remains visible without enabling a source', await page.getByRole('button', { name: 'Review source', exact: true }).isDisabled())
  await page.unroute('**/native/v1/sources/accounts')
  await page.getByRole('button', { name: 'Refresh sources', exact: true }).click()
  await page.getByText('No source is available to this desktop yet. During setup, explicitly allow this installation’s app client.', { exact: true }).waitFor()
  for (const [width, height] of [[1440, 900], [960, 600], [390, 740]]) {
    await page.setViewportSize({ width, height }); await page.waitForTimeout(150)
    const dims = await page.evaluate(() => ({ sw: document.documentElement.scrollWidth, sh: document.documentElement.scrollHeight }))
    check(`source panels remain contained at ${width}x${height}`, dims.sw <= width + 1 && dims.sh <= height + 1)
    const overflow = await page.locator('.runs-card input, .runs-card select, .runs-card textarea, .runs-card button').evaluateAll(nodes => nodes.filter(node => {
      const box = node.getBoundingClientRect(), card = node.closest('.runs-card').getBoundingClientRect()
      return box.width > 0 && (box.left < card.left - 1 || box.right > card.right + 1)
    }).map(node => node.getAttribute('aria-label') || node.textContent || node.tagName))
    check(`form controls stay inside their cards at ${width}x${height}: ${overflow.join(', ')}`, overflow.length === 0)
    await page.screenshot({ path: join(output, `sources-${width}.png`) })
  }
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, live_source_accounts: false, account_grants_made: 0, platform: process.platform }, null, 2))
} catch (error) {
  const page = ui?.page
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), checks, text: await page?.locator('body').innerText().catch(() => ''), engine_stderr: h?.stderr() }, null, 2))
  await page?.screenshot({ path: join(output, 'failure.png') }).catch(() => {})
  console.error(String(error)); throw error
} finally { await ui?.close(); await h?.stop() }

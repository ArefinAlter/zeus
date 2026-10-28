// Native daemon + real UI. Samples a public page, creates a pending run, and cancels it.
// Never approves a run, opens an approval terminal, or installs a grant.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

if (!process.env.ZEUS_RUNS_EVIDENCE) throw new Error('Set ZEUS_RUNS_EVIDENCE to a fresh scratch directory')
const output = resolve(process.env.ZEUS_RUNS_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a fresh scratch profile')
mkdirSync(output, { recursive: true })
const pageUrl = process.env.ZEUS_WORKFLOW_PUBLIC_PAGE ?? 'https://zeus-site-bay.vercel.app/get-started/'
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  const request = (op, body = {}) => h.post(`/native/v1/workflows/${op}`, body)
  const refused = async (name, f) => { let denied = false; try { await f() } catch (e) { denied = e.status >= 400 && !!e.code } check(name, denied) }
  check('native paged workflow list starts empty', (await request('list', { limit: 50 })).items.length === 0)
  await refused('workflow routes cannot decide approvals', () => request('approvals.decide', { decision: 'approve' }))
  await refused('native public-page sampler refuses loopback', () => request('plan', { goal: 'refusal fixture', start_urls: ['http://127.0.0.1/'], idempotency_key: 'loopback-refusal' }))
  ui = await openUi(h)
  const page = ui.page
  page.on('response', async response => {
    if (response.url().endsWith('/native/v1/workflows/plan')) {
      const data = await response.json().catch(() => null)
      writeFileSync(join(output, 'public-plan-response.json'), JSON.stringify({ request: response.request().postDataJSON(), response: data }, null, 2))
    }
  })
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/runs`)
  await page.getByLabel('Workflow goal').fill('Native workflow acceptance')
  await page.getByLabel('Workflow page URLs').fill(pageUrl)
  await page.getByRole('button', { name: 'Create starting plan', exact: true }).click()
  const editor = page.getByLabel('Plan JSON', { exact: true })
  await until(async () => (await editor.inputValue()).includes('plan_version'))
  const plan = JSON.parse(await editor.inputValue())
  plan.extract.items.query = process.env.ZEUS_WORKFLOW_PUBLIC_SELECTOR ?? 'a[href*="/releases/download/"][href$="SHA256SUMS.txt"]'
  plan.allowed_domains.push('github.com', 'release-assets.githubusercontent.com')
  plan.budgets = { max_pages: 2, max_files: 2, max_bytes: 1048576, max_duration_ms: 60000, max_depth: 1 }
  await editor.fill(JSON.stringify(plan, null, 2))
  await page.getByRole('button', { name: 'Validate and sample pages', exact: true }).click()
  await page.getByRole('button', { name: 'Dry run', exact: true }).waitFor()
  await until(async () => {
    if (await page.getByRole('alert').count()) throw new Error(await page.getByRole('alert').first().innerText())
    const validation = page.getByLabel('Plan validation')
    if (await validation.count() && (await validation.innerText()).includes('Plan needs changes')) throw new Error(await validation.innerText())
    return await page.getByRole('button', { name: 'Dry run', exact: true }).isEnabled()
  }, { timeoutMs: 90000, what: 'public plan validation' })
  check('public plan is sampled and normalized by the native engine', (await page.getByLabel('Plan validation').innerText()).includes('Validated plan'))
  check('approval request stays disabled before a dry run', await page.getByRole('button', { name: 'Request run approval', exact: true }).isDisabled())
  await page.getByRole('button', { name: 'Save exact plan', exact: true }).click()
  await page.getByText('Exact validated plan saved on this computer. Reopening it requires a fresh validation and dry run.', { exact: true }).waitFor()
  const saved = (await h.get('/svc/v1/state')).slices.workflow_drafts.value[0]
  check('saved plan is the engine-normalized JSON with its exact hash', JSON.stringify(JSON.parse(saved.text)) === JSON.stringify(JSON.parse(await editor.inputValue())) && saved.plan_hash.length === 64)
  await page.getByRole('button', { name: 'Dry run', exact: true }).click()
  await until(async () => await page.getByRole('button', { name: 'Request run approval', exact: true }).isEnabled(), { timeoutMs: 90000, what: 'complete matching dry run' })
  check('native dry run exposes source and filename samples', (await page.getByLabel('Dry run report').innerText()).includes('Sampling complete'))
  await refused('stale plan hashes cannot request a run', () => request('run', { plan_id: saved.plan_id, plan_hash: '0'.repeat(64), idempotency_key: 'stale-hash' }))
  await page.getByRole('button', { name: 'Request run approval', exact: true }).click()
  const detail = page.getByRole('region', { name: 'Workflow run details' })
  await detail.getByRole('button', { name: 'Open approval review', exact: true }).waitFor({ timeout: 30000 })
  const run = (await request('list', { limit: 50 })).items[0]
  check('run remains pending and no downloads begin without human approval', run.status === 'awaiting_approval' && run.approval_id && run.files_enqueued === 0 && (await h.get('/api/v1/jobs')).items.length === 0)
  check('pause and resume do not offer invalid pending-run actions', await detail.getByRole('button', { name: 'Pause run', exact: true }).isDisabled() && await detail.getByRole('button', { name: 'Resume run', exact: true }).isDisabled())
  const replayBody = { plan_id: saved.plan_id, plan_hash: saved.plan_hash, idempotency_key: 'native-pending-retry' }
  // A separate intentionally pending run verifies retry receipts; neither run is approved.
  const retry1 = await request('run', replayBody), retry2 = await request('run', replayBody)
  check('lost-response retry key resolves to the same pending run', retry1.run_id === retry2.run_id && retry2.replayed)
  await request('control', { run_id: retry1.run_id, action: 'cancel', idempotency_key: 'cancel-retry-fixture' })
  await detail.getByRole('button', { name: 'Cancel run', exact: true }).click()
  await until(async () => (await request('get', { run_id: run.run_id })).status === 'canceled')
  check('native run control persists cancellation', true)
  await until(async () => (await detail.innerText()).includes('canceled'))
  await detail.getByRole('button', { name: 'Export CSV manifest', exact: true }).click()
  const exported = page.getByLabel('Exported workflow manifest')
  await exported.waitFor()
  const exportPath = await exported.locator('code').first().innerText()
  check('manifest export writes the bounded report to the actual engine destination', existsSync(exportPath) && readFileSync(exportPath, 'utf8').length > 0)
  // A forced UI read failure must be visible and retain the previous successful rows.
  await page.route('**/native/v1/workflows/list', route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { code: 'fixture.offline', message: 'Injected workflow list failure' } }) }))
  await page.getByRole('button', { name: 'Refresh runs', exact: true }).click()
  await page.getByText(/Injected workflow list failure/).waitFor()
  check('list failures remain visible without clearing existing runs', await page.getByRole('button', { name: run.run_id, exact: true }).count() === 1)
  await page.unroute('**/native/v1/workflows/list')
  for (const [width, height] of [[1440, 900], [960, 600], [390, 740]]) {
    await page.setViewportSize({ width, height }); await page.waitForTimeout(150)
    const dims = await page.evaluate(() => ({ w: innerWidth, h: innerHeight, sw: document.documentElement.scrollWidth, sh: document.documentElement.scrollHeight }))
    check(`workflow panels remain contained at ${width}x${height}`, dims.sw <= width + 1 && dims.sh <= height + 1)
    const overflow = await page.locator('.runs-card input, .runs-card select, .runs-card textarea, .runs-card button').evaluateAll(nodes => nodes.filter(node => {
      const box = node.getBoundingClientRect(), card = node.closest('.runs-card').getBoundingClientRect()
      return box.width > 0 && (box.left < card.left - 1 || box.right > card.right + 1)
    }).map(node => node.getAttribute('aria-label') || node.textContent || node.tagName))
    check(`form controls stay inside their cards at ${width}x${height}: ${overflow.join(', ')}`, overflow.length === 0)
    await page.screenshot({ path: join(output, `runs-${width}.png`) })
  }
  await ui.close(); ui = null; await h.stop()
  h = await startHarness({ home }); await h.ready()
  ui = await openUi(h)
  await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/runs`)
  await ui.page.getByLabel('Saved plans').selectOption(saved.plan_id)
  check('reopening a saved plan restores exact text but no execution permission', await ui.page.getByLabel('Plan JSON', { exact: true }).inputValue() === saved.text && await ui.page.getByRole('button', { name: 'Request run approval', exact: true }).isDisabled())
  check('run reports survive engine restart', (await h.post('/native/v1/workflows/get', { run_id: run.run_id })).status === 'canceled')
  const kinds = (await h.get('/native/v1/agent/config')).kinds
  check('five official CLI presets report discovery without auth claims', ['codex', 'claude', 'cursor', 'antigravity', 'grok'].every(name => kinds.some(k => k.kind === name && !k.needs_key && typeof k.installed === 'boolean')))
  await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/settings/providers`)
  await ui.page.getByRole('button', { name: 'Refresh installed agents', exact: true }).waitFor()
  check('provider discovery has a visible refresh control', true)
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, public_page: pageUrl, live_provider_inference: false, run_approvals_made: 0, platform: process.platform }, null, 2))
} catch (error) {
  const page = ui?.page
  writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), checks, text: await page?.locator('body').innerText().catch(() => ''), engine_stderr: h?.stderr() }, null, 2))
  await page?.screenshot({ path: join(output, 'failure.png') }).catch(() => {})
  console.error(String(error)); throw error
} finally { await ui?.close(); await h?.stop() }

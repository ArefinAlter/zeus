// Exact-action authorization through the real desktop and daemon, using only local fixtures.
import assert from 'node:assert/strict'
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { startFixtures, sha256 } from '../fixture-server.mjs'
import { startHarness, until, sleep } from '../harness.mjs'
import { startProvider } from '../providers-mock.mjs'
import { openUi } from '../ui.mjs'

const out = process.env.ZEUS_AGENT_REVIEW_EVIDENCE ?? `D:/verne-build/audit-zeus-desktop/evidence/agent-review-${Date.now()}`
mkdirSync(out, { recursive: false })
const fx = await startFixtures(0), provider = await startProvider(0)
let h, ui
const checks = []
const check = (name, ok) => { assert.ok(ok, name); checks.push(name); console.log(`PASS ${name}`) }
try {
  h = await startHarness({ home: join(out, 'profile'), fresh: true, env: { ZEUS_KEYSTORE: 'file' } })
  await h.ready()
  const B = '/native/v1/agent'
  await h.post(`${B}/keystore/initialize`, { passphrase: 'isolated fixture only 20261031' })
  await h.put(`${B}/config`, {
    providers: [{ id: 'p-review', name: 'Local fixture', kind: 'openai_compatible', enabled: true, consent: true,
      base_url: `${provider.url}/v1`, model: 'mock-model-a', monthly_cap_usd: 5, tools_mode: 'native', price_in_per_mtok: 0, price_out_per_mtok: 0 }],
    offline: false, daily_cap_usd: 5, thread_cap_usd: 2, default_price_in_per_mtok: 0, default_price_out_per_mtok: 0, max_tokens: 2048,
  })
  await h.put(`${B}/keys/p-review`, { key: 'FIXTURE-ONLY-REVIEW-CANARY' })
  const jobs = async () => (await h.get('/api/v1/jobs')).items
  const events = id => h.post(`${B}/runs/${id}/events`, { after: 0 })
  const start = async name => {
    const { run_id } = await h.post(`${B}/send`, { thread_id: `th_${name}`, provider_id: 'p-review', access: 'download',
      text: `download ${fx.url}/files/${name}.bin?size=65536&seed=61`, regenerate: false,
      settings: { tools_enabled: ['zeus_enqueue'], max_turn_tools: 2, max_batch: 1, metadata_only: true, instructions: '' } })
    const review = await until(async () => (await events(run_id)).events.find(e => e.type === 'tool_review'), { what: 'a suspended tool review' })
    return { run_id, review }
  }
  const first = await start('decline')
  await sleep(500)
  check('no job exists before the action is approved', (await jobs()).length === 0)
  const decide = (run, id, approve, extra = {}) => h.post(`${B}/runs/${run}/review`, { review_id: id, approve, ...extra })
  await assert.rejects(decide(first.run_id, '0'.repeat(32), true))
  await assert.rejects(decide(first.run_id, first.review.review_id, true, { arguments: { urls: ['https://example.org/changed'] } }))
  check('wrong review IDs and replacement arguments have no effects', (await jobs()).length === 0)
  const other = await start('other')
  await assert.rejects(decide(other.run_id, first.review.review_id, true))
  await h.post(`${B}/runs/${other.run_id}/stop`)
  await until(async () => (await events(other.run_id)).done, { what: 'canceled run' })
  await assert.rejects(decide(other.run_id, other.review.review_id, true))
  await decide(first.run_id, first.review.review_id, false)
  await until(async () => (await events(first.run_id)).done, { what: 'declined run' })
  await assert.rejects(decide(first.run_id, first.review.review_id, true))
  check('wrong-run, cancellation, decline and replay cannot start a download', (await jobs()).length === 0)

  ui = await openUi(h)
  const page = ui.page
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/agent`)
  await page.waitForSelector('textarea[aria-label="Message to the agent"]')
  await page.click('button[aria-label^="Access:"]')
  await page.click('[role=menuitemradio]:has-text("Download")')
  await page.fill('textarea[aria-label="Message to the agent"]', `download ${fx.url}/files/review-ui.bin?size=65536&seed=62`)
  await page.keyboard.press('Enter')
  await page.getByRole('group', { name: 'Review proposed action' }).waitFor()
  check('the real UI shows the exact pending file with no download started', (await page.locator('.timeline').innerText()).includes('review-ui.bin') && (await jobs()).length === 0)
  await page.screenshot({ path: join(out, 'pending.png') })
  await page.reload()
  await page.getByRole('group', { name: 'Review proposed action' }).waitFor()
  check('a UI reload preserves review without approving it', (await jobs()).length === 0)
  await page.getByRole('button', { name: 'Run this action', exact: true }).click()
  const completed = await until(async () => (await jobs()).find(job => job.state === 'completed'), { what: 'the reviewed download to complete' })
  check('one approved action produces exactly one verified file', (await jobs()).length === 1 && completed.sha256 === sha256(62, 65536))
  await page.getByRole('group', { name: 'Review proposed action' }).waitFor({ state: 'hidden' })
  await page.screenshot({ path: join(out, 'completed.png') })
  check('the UI has no script errors', ui.errors.length === 0)
  writeFileSync(join(out, 'result.json'), JSON.stringify({ checks }, null, 2))
  console.log('SCENARIO 30: PASSED')
} finally {
  await ui?.close()
  await h?.stop()
  await provider.close()
  await fx.close()
}

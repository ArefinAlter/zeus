// Real native journal/cache with an interrupted frontend, using a new explicit scratch profile.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startFixtures, sha256 } from '../fixture-server.mjs'
import { download, startHarness, until, sleep } from '../harness.mjs'
import { openUi } from '../ui.mjs'

if (!process.env.ZEUS_HISTORY_EVIDENCE) throw new Error('Set ZEUS_HISTORY_EVIDENCE to a new scratch evidence directory')
const out = resolve(process.env.ZEUS_HISTORY_EVIDENCE), home = join(out, 'profile')
if (existsSync(home)) throw new Error('Use a new scratch profile')
mkdirSync(out, { recursive: true })
const checks = [], check = (name, ok) => { assert.ok(ok, name); checks.push(name); console.log('PASS ' + name) }
let h, fx, ui, release
try {
  fx = await startFixtures(0)
  h = await startHarness({ home }); await h.ready()
  const initial = await h.get('/native/v1/events')
  check('fresh history negotiates bounded replay without an added daemon listener', !!initial.cursor && !initial.replaying)
  const size = 32 * 1024 * 1024, seed = 47
  const result = await download(h, [{ url: `${fx.url}/files/history.bin?size=${size}&seed=${seed}&rate=256` }])
  const id = result.jobs[0].job_id
  const job = async () => (await h.get('/api/v1/jobs')).items.find(j => j.id === id)
  await until(async () => (await job())?.bytes_done > 1024 * 1024)
  ui = await openUi(h)
  const page = ui.page
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/downloads?job=${id}`)
  const properties = page.getByRole('complementary', { name: 'Properties of history.bin' })
  await properties.waitFor()
  await until(async () => (await properties.innerText()).includes('Source checked'))
  const before = await h.get('/native/v1/events')
  let failures = 0
  await page.route('**/api/v1/jobs', route => { failures++; return route.fulfill({ json: { items: [], failure: { code: 'engine.restarting', message: 'Reconnecting to the test engine.' }, updated_ms: Date.now() } }) })
  await page.route('**/native/v1/events', route => route.fulfill({ status: 503, json: { error: { code: 'engine.restarting', message: 'Reconnecting to the test engine.' } } }))
  await until(() => failures >= 2)
  check('empty failed snapshots retain the existing download and inspector', await properties.isVisible() && await page.getByRole('grid').getByText('history.bin', { exact: true }).count() === 1)
  await until(async () => (await properties.innerText()).includes('Reconnecting to history'))
  check('history read failure preserves previous records and exposes reconnection', (await properties.innerText()).includes('Source checked'))
  await h.post(`/api/v1/jobs/${id}/control`, { action: 'pause', idempotency_key: 'history-pause' })
  await until(async () => (await job())?.state === 'paused')
  await h.post(`/api/v1/jobs/${id}/control`, { action: 'resume', idempotency_key: 'history-resume' })
  const done = await until(async () => { const j = await job(); return j?.state === 'completed' && j }, { timeoutMs: 120000 })
  check('the engine continues through pause, resume and completion while the frontend is disconnected', done.sha256 === sha256(seed, size))
  await page.unroute('**/api/v1/jobs'); await page.unroute('**/native/v1/events')
  await until(async () => (await properties.innerText()).includes('Download completed'))
  check('durable replay recovers a transition missed by frontend polling', (await properties.innerText()).includes('zeusd: paused'))
  const recovered = await h.get('/native/v1/events')
  const entries = recovered.entries.filter(e => e.job_id === id)
  check('replayed history remains ordered and deduplicated', entries.every((e, n) => !n || e.seq > entries[n-1].seq) && new Set(entries.map(e => e.seq)).size === entries.length)
  check('cursor advances beyond the disconnected window', recovered.cursor.seq > before.cursor.seq)
  const cachePath = join(home, 'app/history/events.json')
  const saved = JSON.parse(readFileSync(cachePath, 'utf8'))
  check('cursor and entries are saved in one bounded cache without URLs or raw event data', saved.cursor.seq === recovered.cursor.seq && !JSON.stringify(saved).includes(fx.url) && !JSON.stringify(saved).includes('data') && readFileSync(cachePath).length < 1024 * 1024)

  // Hold one snapshot across several poll intervals. Only one request may be in flight.
  let calls = 0
  const gate = new Promise(resolve => { release = resolve })
  await page.route('**/api/v1/jobs', async route => { calls++; await gate; await route.fulfill({ json: { items: [done], updated_ms: Date.now(), failure: null } }) })
  await until(() => calls === 1); await sleep(5000)
  check('slow job reads do not launch overlapping snapshots', calls === 1)
  release(); release = null
  await sleep(300); await page.unroute('**/api/v1/jobs')
  await page.reload(); await properties.waitFor()
  await until(async () => (await properties.innerText()).includes('zeusd: paused'))
  check('a webview reload restores recorded history', (await properties.innerText()).includes('Download completed'))
  await properties.getByRole('heading', { name: 'History', exact: true }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: join(out, 'recovered-history.png'), fullPage: true })
  check('the UI has no unexpected script or console errors', ui.errors.filter(e => !e.includes('503 (Service Unavailable)')).length === 0)
  await ui.close(); ui = null
  const oldInstance = recovered.cursor.instance_id
  await h.stop(); h = await startHarness({ home }); await h.ready()
  const restarted = await h.get('/native/v1/events')
  check('a daemon restart binds a new cursor and replays retained history', restarted.cursor.instance_id !== oldInstance && restarted.entries.some(e => e.job_id === id && e.state === 'paused'))
  check('replay after restart cannot regress the completed job snapshot', (await job()).state === 'completed' && (await job()).sha256 === done.sha256)
  const sequence = restarted.entries.filter(e => e.job_id === id).map(e => e.seq)
  check('restart replay has no duplicate completion history', new Set(sequence).size === sequence.length)
  await h.stop(); h = null
  writeFileSync(cachePath, '{partial-write')
  h = await startHarness({ home }); await h.ready()
  const repaired = await h.get('/native/v1/events')
  check('a corrupt display cache is rebuilt from the journal', repaired.entries.some(e => e.job_id === id && e.kind === 'job.completed') && (await job()).state === 'completed')
  const denied = await h.post('/native/v1/events', { method: 'approvals.decide' }).then(() => false, () => true)
  check('the history route offers no mutation or approval forwarding', denied)
  writeFileSync(join(out, 'result.json'), JSON.stringify({ checks, job: id, history_entries: repaired.entries.length }, null, 2))
} finally {
  release?.()
  await ui?.close()
  await h?.stop()
  await fx?.close()
}

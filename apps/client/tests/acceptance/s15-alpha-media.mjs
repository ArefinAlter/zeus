// Finite media review and optional fixed mux through the real native bridge/UI.
// Every profile, source, helper pin and destination belongs to this fixture.
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const output = process.env.ZEUS_ALPHA_MEDIA_EVIDENCE
const fixtures = process.env.ZEUS_MEDIA_TEST_FIXTURES
if (!output || !fixtures) throw new Error('isolated media evidence directory and synthetic fixtures are required')
const home = resolve(join(output, 'profile'))
if (existsSync(home)) rmSync(home, { recursive: true, force: true })
mkdirSync(join(home, 'app'), { recursive: true })
const helper = process.env.ZEUS_MEDIA_TEST_HELPER
if (helper) writeFileSync(join(home, 'app', 'media-helper.json'), JSON.stringify({ version: 1, enabled: true, executable: helper, sha256: createHash('sha256').update(readFileSync(helper)).digest('hex') }))
const data = { video: readFileSync(join(fixtures, 'video.mp4')), audio: readFileSync(join(fixtures, 'audio.m4a')) }
let sourceRequests = 0
const source = createServer((req, res) => {
  sourceRequests++
  const path = new URL(req.url, 'http://fixture').pathname
  const playlist = (text) => { res.writeHead(200, { 'content-type': 'application/vnd.apple.mpegurl', 'content-length': Buffer.byteLength(text) }); res.end(req.method === 'HEAD' ? undefined : text) }
  if (path === '/master.m3u8') return playlist('#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=128000,RESOLUTION=64x64\n/video.m3u8\n')
  if (path === '/video.m3u8') return playlist(`#EXTM3U\n#EXT-X-MAP:URI="/video.mp4",BYTERANGE="16@0"\n#EXTINF:1,\n#EXT-X-BYTERANGE:${data.video.length - 16}@16\n/video.mp4\n#EXT-X-ENDLIST\n`)
  const bytes = path === '/video.mp4' ? data.video : path === '/audio.m4a' ? data.audio : null
  if (!bytes) { res.writeHead(404); return res.end() }
  const range = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range ?? '')
  const start = range ? Number(range[1]) : 0
  const end = range ? Number(range[2]) : bytes.length - 1
  res.writeHead(range ? 206 : 200, { 'content-type': path.endsWith('m4a') ? 'audio/mp4' : 'video/mp4', 'content-length': end - start + 1, etag: '"media-ui-v1"', 'accept-ranges': 'bytes', ...(range ? { 'content-range': `bytes ${start}-${end}/${bytes.length}` } : {}) })
  res.end(req.method === 'HEAD' ? undefined : bytes.subarray(start, end + 1))
})
await new Promise((resolve) => source.listen(0, '127.0.0.1', resolve))
const base = `http://127.0.0.1:${source.address().port}`
const checks = []
const check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  ui = await openUi(h, { width: 1360, height: 880 })
  await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/grabber`)
  await ui.page.getByRole('button', { name: 'Media grabber · HLS, DASH and direct media', exact: true }).click()
  const panel = ui.page.getByRole('region', { name: 'Media grabber', exact: true })
  await panel.getByLabel('Media or page URL', { exact: true }).fill(`${base}/master.m3u8?signature=ZEUS-CANARY-source`)
  await panel.getByRole('button', { name: 'Inspect media', exact: true }).click()
  await panel.getByRole('button', { name: 'Select and review', exact: true }).click()
  await panel.getByLabel('File name', { exact: true }).fill('fixture-video.mp4')
  check('finite HLS expansion is reviewed before any media task exists', (await h.get('/native/v1/media/tasks')).items.length === 0)
  await panel.getByRole('button', { name: 'Start reviewed media', exact: true }).click()
  const video = await until(async () => { const task = (await h.get('/native/v1/media/tasks')).items.find(task => task.filename === 'fixture-video.mp4'); if (task?.state === 'failed') throw new Error(task.error_message); return task?.state === 'complete' && task }, { what: 'reviewed HLS output' })
  check('actual HLS ranges produce the exact video with SHA256 in the UI', video.sha256 === createHash('sha256').update(data.video).digest('hex'))
  await panel.getByText(`SHA-256 ${video.sha256}`, { exact: true }).waitFor()
  check('source input is cleared after dispatch', await panel.getByLabel('Media or page URL', { exact: true }).inputValue() === '')
  check('signed source address is absent from browser storage', await ui.page.evaluate(() => ![...Object.values(localStorage), ...Object.values(sessionStorage)].some(value => value.includes('ZEUS-CANARY'))))
  const projection = await h.get('/native/v1/media/tasks')
  check('native persistent projection omits source addresses and signatures', !JSON.stringify(projection).includes('ZEUS-CANARY') && !JSON.stringify(projection).includes(base))
  await panel.getByLabel('Media or page URL', { exact: true }).fill(`${base}/audio.m4a`)
  await panel.getByRole('button', { name: 'Inspect media', exact: true }).click()
  await panel.getByRole('button', { name: 'Select and review', exact: true }).click()
  await panel.getByLabel('File name', { exact: true }).fill('fixture-audio.m4a')
  await panel.getByRole('button', { name: 'Start reviewed media', exact: true }).click()
  const audio = await until(async () => { const task = (await h.get('/native/v1/media/tasks')).items.find(task => task.filename === 'fixture-audio.m4a'); if (task?.state === 'failed') throw new Error(task.error_message); return task?.state === 'complete' && task }, { what: 'direct audio output' })
  check('direct audio acquisition retains the exact bytes', audio.sha256 === createHash('sha256').update(data.audio).digest('hex'))
  if (helper) {
    const mux = panel.getByRole('region', { name: 'Local media mux', exact: true })
    await mux.getByLabel('Media or video track', { exact: true }).selectOption(video.task_id)
    await mux.getByLabel('Separate audio track, optional', { exact: true }).selectOption(audio.task_id)
    await mux.getByRole('button', { name: 'Review stream-copy operation', exact: true }).click()
    await mux.getByLabel('Output filename', { exact: true }).fill('fixture-combined.mp4')
    check('mux helper identity is visible before explicit start', await mux.getByText(/Helper SHA-256/).count() === 1)
    await mux.getByRole('button', { name: 'Start reviewed stream copy', exact: true }).click()
    const combined = await until(async () => { const task = (await h.get('/native/v1/media/tasks')).items.find(task => task.filename === 'fixture-combined.mp4'); if (task?.state === 'failed') throw new Error(task.error_message); return task?.state === 'complete' && task }, { what: 'real fixed-profile helper output' })
    check('real UI starts the bounded helper and publishes a verified combined output', combined.kind === 'mux' && combined.sha256?.length === 64 && combined.source_task_ids.length === 2)
  }
  check('media interface has no page errors', ui.errors.length === 0)
  await ui.page.screenshot({ path: join(output, 'media-grabber.png'), fullPage: true })
  writeFileSync(join(output, 'results.json'), JSON.stringify({ passed: true, checks, sourceRequests, pageErrors: ui.errors, helperAcceptance: !!helper }, null, 2))
} catch (error) {
  if (ui) { writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), body: await ui.page.locator('body').innerText(), pageErrors: ui.errors }, null, 2)); await ui.page.screenshot({ path: join(output, 'failure.png'), fullPage: true }) }
  throw error
} finally {
  await ui?.close(); await h?.stop(); await new Promise((resolve) => source.close(resolve))
}

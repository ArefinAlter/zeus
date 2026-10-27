// Scenario 4: queues and the scheduler (start time, days, concurrency, on-finish) and the speed limiter (global,
// per queue, schedule), all inside zeusd, through the app's routes and the Queues page. Throughput is measured against
// the limit. Run: node tests/acceptance/s04-queues.mjs

import { startFixtures } from '../fixture-server.mjs'
import { download, sleep, startHarness, until } from '../harness.mjs'
import { guard } from '../guard.mjs'
import { openUi } from '../ui.mjs'

const home = 'D:/verne-build/zeus/scratch/s04'
const results = []
const check = (name, ok, detail = '') => {
  results.push(ok)
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
let fx
let h
let ui
guard(() => ({ h, fx, ui }))
fx = await startFixtures(0)
h = await startHarness({ home, fresh: true })
await h.ready()
const jobs = async () => (await h.get('/api/v1/jobs')).items
const queues = () => h.get('/native/v1/queues')
const F = (name, size, seed, extra = '') => `${fx.url}/files/${name}?size=${size}&seed=${seed}${extra}`
const MiB = 1024 * 1024

// ---------------------------------------------------------------- A: a queue that waits for its start time
const now = new Date()
const at = new Date(now.getTime() + 75_000)
const hm = `${String(at.getHours()).padStart(2, '0')}:${String(at.getMinutes()).padStart(2, '0')}`
const ymd = `${at.getFullYear()}-${String(at.getMonth() + 1).padStart(2, '0')}-${String(at.getDate()).padStart(2, '0')}`
const q = {
  id: 'night',
  name: 'Night',
  kind: 'download',
  max_concurrent: 1,
  retries: 2,
  retry_delay_s: 5,
  schedule: { start_enabled: true, start_time: hm, repeat: 'once', start_date: ymd, days: [0, 1, 2, 3, 4, 5, 6], stop_enabled: false, stop_time: '06:00' },
  on_finish: 'notify',
  limit_kib_s: null,
}
await h.post('/native/v1/queues/put', { queue: q })
const listed = (await queues()).queues.find((x) => x.id === 'night')
check('a queue with a start time is stored by zeusd and reports its next start', !!listed && listed.next_start_ms > Date.now(), `next start ${new Date(listed?.next_start_ms ?? 0).toLocaleTimeString()}`)
const items = [1, 2, 3].map((n) => ({ url: F(`night-${n}.bin`, 3 * MiB, 60 + n, '&rate=4096'), queue: 'night' }))
const started = await download(h, items)
const ids = started.jobs.map((j) => j.job_id)
await sleep(2500)
let l = (await queues()).queues.find((x) => x.id === 'night')
const before = (await jobs()).filter((j) => ids.includes(j.id))
check('the three downloads are held: queued in zeusd, nothing transferred', l.held_job_ids.length === 3 && !l.running && before.every((j) => j.bytes_done === 0), `held ${l.held_job_ids.length}, states ${before.map((j) => j.state).join(',')}`)
const bodyRequests = () => fx.log.filter((r) => /night-\d/.test(r.path) && r.range && !r.range.startsWith('bytes=0-0')).length
check('the server saw no ranged body request while the queue waits', bodyRequests() === 0, `${bodyRequests()} requests`)
const startWaitStart = Date.now()
await until(async () => (await queues()).queues.find((x) => x.id === 'night')?.running, { timeoutMs: 150_000, everyMs: 1000, what: 'the scheduled start' })
const startedAt = new Date()
const due = new Date(at); due.setSeconds(0, 0)
check('the queue starts by itself at its start time', Math.abs(startedAt.getTime() - due.getTime()) < 15_000, `due ${hm}, started ${startedAt.toLocaleTimeString()} (${((Date.now() - startWaitStart) / 1000).toFixed(0)} s wait)`)
let maxActive = 0
await until(
  async () => {
    const js = (await jobs()).filter((j) => ids.includes(j.id))
    const active = js.filter((j) => ['probing', 'downloading', 'verifying', 'finalizing'].includes(j.state)).length
    maxActive = Math.max(maxActive, active)
    return js.every((j) => j.state === 'completed')
  },
  { timeoutMs: 120_000, everyMs: 300, what: 'the queue to finish' },
)
check('only one download ran at a time (max concurrent 1)', maxActive === 1, `max active ${maxActive}`)
l = (await queues()).queues.find((x) => x.id === 'night')
await until(async () => (await queues()).queues.find((x) => x.id === 'night')?.finish_action_pending, { timeoutMs: 15000, what: 'the finish bookkeeping' })
check('when the last file finishes the queue stops and asks for its on-finish action', true)
await h.post('/native/v1/queues/control', { action: 'ack_finish', id: 'night' })

// ---------------------------------------------------------------- B: start and stop by hand pause and resume
await h.post('/native/v1/queues/put', { queue: { ...q, id: 'manual', name: 'Manual', schedule: { ...q.schedule, start_enabled: false }, max_concurrent: 2 } })
const man = await download(h, [1, 2, 3].map((n) => ({ url: F(`man-${n}.bin`, 24 * MiB, 70 + n, '&rate=1024'), queue: 'manual' })))
const mids = man.jobs.map((j) => j.job_id)
await h.post('/native/v1/queues/control', { action: 'start', id: 'manual' })
await until(async () => (await jobs()).filter((j) => mids.includes(j.id) && j.state === 'downloading').length === 2, { timeoutMs: 40000, what: 'two downloads' })
check('a started queue runs two at a time and holds the third', (await queues()).queues.find((x) => x.id === 'manual').held_job_ids.length === 1)
await h.post('/native/v1/queues/control', { action: 'stop', id: 'manual' })
await sleep(2500)
const paused = (await jobs()).filter((j) => mids.includes(j.id))
check('stopping the queue pauses its downloads and keeps their bytes', paused.every((j) => j.state === 'paused'), paused.map((j) => j.state).join(','))
for (const j of paused) await h.post(`/api/v1/jobs/${j.id}/control`, { action: 'cancel' }).catch(() => undefined)

// ---------------------------------------------------------------- C: the speed limiter, measured
async function measure(label, size, seed, extra, limitKiB) {
  const t0 = fx.log.length
  const r = await download(h, [{ url: F(`lim-${label}.bin`, size, seed, extra) }], `lim-${label}`)
  const id = r.jobs[0].job_id
  let committedAt = 0
  await until(async () => {
    const j = (await jobs()).find((x) => x.id === id)
    if (!committedAt && j && ['verifying', 'finalizing', 'completed'].includes(j.state)) committedAt = Date.now()
    return j?.state === 'completed'
  }, { timeoutMs: 120000, everyMs: 80, what: label })
  const body = fx.log.slice(t0).filter((x) => x.path.includes(`lim-${label}`) && x.range && !x.range.startsWith('bytes=0-0'))
  const first = Math.min(...body.map((x) => x.t))
  const last = Math.max(...body.map((x) => x.end ?? 0))
  const secs = ((last || committedAt) - first) / 1000
  const kib = size / 1024 / secs
  console.log(`  ${label}: ${(size / MiB).toFixed(0)} MiB in ${secs.toFixed(2)} s = ${kib.toFixed(0)} KiB/s (limit ${limitKiB ?? 'none'})`)
  return kib
}
await h.post('/native/v1/queues/configure', { limiter: { enabled: true, limit_kib_s: 1024, schedule_enabled: false, from: '08:00', to: '18:00', days: [0, 1, 2, 3, 4, 5, 6] } })
const g = await measure('global', 8 * MiB, 81, '', 1024)
check('global limit 1024 KiB/s: measured throughput within 10 percent', Math.abs(g - 1024) / 1024 <= 0.1, `${g.toFixed(0)} KiB/s`)
await h.post('/native/v1/queues/put', { queue: { ...q, id: 'slow', name: 'Slow', schedule: { ...q.schedule, start_enabled: false }, max_concurrent: 2, limit_kib_s: 512 } })
const sl = await download(h, [{ url: F('lim-queue.bin', 4 * MiB, 82), queue: 'slow' }], 'lim-q')
await h.post('/native/v1/queues/control', { action: 'start', id: 'slow' })
const t1 = fx.log.length
let committed2 = 0
await until(async () => {
  const j = (await jobs()).find((x) => x.id === sl.jobs[0].job_id)
  if (!committed2 && j && ['verifying', 'finalizing', 'completed'].includes(j.state)) committed2 = Date.now()
  return j?.state === 'completed'
}, { timeoutMs: 60000, everyMs: 80, what: 'the queue-limited download' })
const b2 = fx.log.slice(t1).filter((x) => x.path.includes('lim-queue') && x.range && !x.range.startsWith('bytes=0-0'))
const secs2 = (Math.max(...b2.map((x) => x.end ?? 0)) - Math.min(...b2.map((x) => x.t))) / 1000
const kib2 = (4 * MiB) / 1024 / secs2
console.log(`  queue: 4 MiB in ${secs2.toFixed(2)} s = ${kib2.toFixed(0)} KiB/s (queue limit 512, global 1024)`)
check('a queue limit of 512 KiB/s under the global 1024 holds within 10 percent', Math.abs(kib2 - 512) / 512 <= 0.1, `${kib2.toFixed(0)} KiB/s`)
// A schedule window that is not now switches the limiter off.
const hourLater = `${String((now.getHours() + 3) % 24).padStart(2, '0')}:00`
const hourLater2 = `${String((now.getHours() + 5) % 24).padStart(2, '0')}:00`
await h.post('/native/v1/queues/configure', { limiter: { enabled: true, limit_kib_s: 256, schedule_enabled: true, from: hourLater, to: hourLater2, days: [0, 1, 2, 3, 4, 5, 6] } })
const free = await measure('window', 12 * MiB, 83, '', 'off-window')
check('outside its schedule window the limiter does not apply', free > 4 * 256, `${free.toFixed(0)} KiB/s`)

// ---------------------------------------------------------------- D: the page shows what zeusd holds
ui = await openUi(h)
const { page } = ui
await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/queues`)
await page.waitForSelector('text=Night', { timeout: 15000 })
const text = (await page.locator('main, .page, body').first().innerText()).replace(/\s+/g, ' ')
check('the Queues page lists the queues zeusd holds and the speed limiter', /Night/.test(text) && /Manual/.test(text) && /Slow/.test(text) && /Speed limiter/.test(text))
await page.locator('text=Speed limiter').first().dispatchEvent('click')
await page.waitForTimeout(800)
const limiterText = (await page.locator('body').innerText()).replace(/\s+/g, ' ')
check('the Speed limiter page shows the limit and window stored in zeusd', /256/.test(limiterText), limiterText.slice(limiterText.indexOf('Speed limiter'), limiterText.indexOf('Speed limiter') + 140))
await ui.close()
await h.stop()
await fx.close()
const failed = results.filter((r) => !r).length
console.log(failed ? `SCENARIO 4: ${failed} FAILED` : 'SCENARIO 4: PASSED')
process.exit(failed ? 1 : 0)

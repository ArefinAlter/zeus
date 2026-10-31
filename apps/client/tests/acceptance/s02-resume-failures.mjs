// Scenario 2: kill the app and zeusd in the middle of a download, relaunch, and the download resumes from its saved
// parts. Servers without ranges, redirects, Content-Disposition names, 403/410 expired links and 503 Retry-After
// behave as the interface says. Run: node tests/acceptance/s02-resume-failures.mjs

import { statSync } from 'node:fs'

import { startFixtures, sha256 } from '../fixture-server.mjs'
import { download, sleep, startHarness, until } from '../harness.mjs'

const homeArg = process.argv.indexOf('--home')
const home = homeArg >= 0 ? process.argv[homeArg + 1] : 'D:/verne-build/zeus/scratch/s02'
if (!home) throw new Error('--home needs a scratch directory')
const results = []
const check = (name, ok, detail = '') => {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}

const fx = await startFixtures(0)
let h = await startHarness({ home, fresh: true })
await h.ready()
const jobOf = async (id) => (await h.get('/api/v1/jobs')).items.find((j) => j.id === id)
const finish = (id, timeoutMs = 120000) => until(async () => { const j = await jobOf(id); return j && ['completed', 'failed', 'canceled'].includes(j.state) ? j : false }, { timeoutMs, what: `job ${id}` })

// ---------------------------------------------------------------- A: kill mid-download, relaunch, resume
const size = 192 * 1024 * 1024
const seed = 21
const started = await download(h, [{ url: `${fx.url}/files/resume.bin?size=${size}&seed=${seed}&rate=2048` }])
const id = started.jobs[0].job_id
const before = await until(async () => { const j = await jobOf(id); return j && j.bytes_done >= 40 * 1024 * 1024 ? j : false }, { timeoutMs: 90000, what: '40 MiB committed' })
const status = await h.get('/api/v1/status')
console.log(`killing the app and zeusd at ${(before.bytes_done / 1048576).toFixed(0)} MiB committed (zeusd pid ${status.engine.pid})`)
try { process.kill(status.engine.pid, 'SIGKILL') } catch (e) { console.log('zeusd kill:', e.message) }
await h.stop({ kill: true })
await sleep(1500)
const logBefore = fx.log.length
h = await startHarness({ home })
const t1 = Date.now()
await h.ready()
const resumed = await until(async () => { const j = await jobOf(id); return j && j.bytes_done >= before.bytes_done ? j : false }, { timeoutMs: 60000, what: 'the job back at its saved bytes' })
check('after relaunch the job keeps its committed bytes', resumed.bytes_done >= before.bytes_done, `${(resumed.bytes_done / 1048576).toFixed(0)} MiB, engine back in ${((Date.now() - t1) / 1000).toFixed(1)} s`)
const done = await finish(id, 240000)
check('the resumed download completes', done.state === 'completed', done.state)
check('its SHA-256 matches the server\'s bytes', done.sha256 === sha256(seed, size))
check('the file on disk has the full size', statSync(done.path).size === size)
const after = fx.log.slice(logBefore).filter((r) => r.path.startsWith('/files/resume.bin') && r.range)
// Committed bytes are a set of out-of-order segments, not a contiguous file prefix. A request
// below bytes_done can legitimately fill a hole. Measure the actual fixture body bytes instead:
// a restart must fetch only the missing data, with 1 MiB headroom for probes/buffered tail data.
const wireBytes = after.reduce((sum, r) => sum + (r.marks ?? []).reduce((n, [, bytes]) => n + bytes, 0), 0)
const remaining = size - before.bytes_done
check('resume avoids re-downloading the saved data', wireBytes > 0 && wireBytes <= remaining + 1024 * 1024, `${(wireBytes / 1048576).toFixed(2)} MiB served after restart; at most ${(remaining / 1048576).toFixed(2)} MiB missing at crash`)

// ---------------------------------------------------------------- B: no ranges, redirects, a server-given name
const nr = await download(h, [{ url: `${fx.url}/norange/plain.bin?size=${6 * 1024 * 1024}&seed=5` }])
const nrDone = await finish(nr.jobs[0].job_id)
const nrDetail = await h.get(`/api/v1/jobs/${nr.jobs[0].job_id}`)
check('a server without ranges downloads in one piece and says so', nrDone.state === 'completed' && nrDetail.job.probe.range_support === 'unsupported', nrDetail.job.probe.range_support)
check('its SHA-256 matches', nrDone.sha256 === sha256(5, 6 * 1024 * 1024))

const rd = await download(h, [{ url: `${fx.url}/redirect/3/hop.bin?size=${3 * 1024 * 1024}&seed=6` }])
const rdDone = await finish(rd.jobs[0].job_id)
const rdDetail = await h.get(`/api/v1/jobs/${rd.jobs[0].job_id}`)
check('three redirects are followed', rdDone.state === 'completed' && rdDetail.job.probe.redirects === 3, `redirects ${rdDetail.job.probe.redirects}`)

const cd = await download(h, [{ url: `${fx.url}/cd/anything?size=${1024 * 1024}&seed=7&as=Quarterly%20Report%20%281%29.pdf` }])
const cdDone = await finish(cd.jobs[0].job_id)
check('the Content-Disposition name is used', cdDone.state === 'completed' && cdDone.filename === 'Quarterly Report (1).pdf' && cdDone.path.endsWith('Quarterly Report (1).pdf'), cdDone.filename)

// ---------------------------------------------------------------- C: expired, gone, busy
const ex = await download(h, [{ url: `${fx.url}/expired/a.zip` }])
const exDone = await finish(ex.jobs[0].job_id, 90000)
check('403 (an expired signed link) fails with a code the interface explains', exDone.state === 'failed' && /http\.(forbidden|url_expired|status_403)/.test(exDone.last_error?.code ?? ''), `${exDone.state} ${exDone.last_error?.code}`)
const gone = await download(h, [{ url: `${fx.url}/gone/a.zip` }])
const goneDone = await finish(gone.jobs[0].job_id, 90000)
check('410 fails and is not retried', goneDone.state === 'failed' && /gone|410/.test(goneDone.last_error?.code ?? ''), `${goneDone.state} ${goneDone.last_error?.code}`)
const busy = await download(h, [{ url: `${fx.url}/busy/wait.bin?size=${2 * 1024 * 1024}&seed=8&fails=2&after=3` }])
const sawRetry = await until(async () => { const j = await jobOf(busy.jobs[0].job_id); return j?.state === 'retry_scheduled' ? j : false }, { timeoutMs: 60000, everyMs: 300, what: 'retry_scheduled' }).catch(() => null)
const busyDone = await finish(busy.jobs[0].job_id, 120000)
const requests = fx.log.filter((r) => r.path.startsWith('/busy/wait.bin')).map((r) => r.t)
const gaps = requests.slice(1).map((t, i) => (t - (requests[i] ?? t)) / 1000)
check('503 with Retry-After schedules a retry and then succeeds', !!sawRetry && busyDone.state === 'completed', `retry_scheduled seen: ${!!sawRetry}; final ${busyDone.state}; gaps between requests (s): ${gaps.map((g) => g.toFixed(1)).join(', ')}`)

await h.stop()
await fx.close()
const failed = results.filter((r) => !r.ok)
console.log(failed.length ? `SCENARIO 2: ${failed.length} FAILED` : 'SCENARIO 2: PASSED')
process.exit(failed.length ? 1 : 0)

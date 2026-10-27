// Scenario 1: a fresh profile, no daemon started by hand. A real segmented, resumable download with live progress,
// speed, ETA and SHA-256, which then appears in the file list. Run: node tests/acceptance/s01-download.mjs [--url URL]
//
// With no --url it uses the local fixture server (a 96 MiB file served at 12 MiB/s per connection). With a real public
// URL (for example https://proof.ovh.net/files/10Mb.dat) it downloads that and reports the same facts.

import { createHash } from 'node:crypto'
import { createReadStream, statSync } from 'node:fs'
import { join } from 'node:path'

import { startFixtures, sha256 } from '../fixture-server.mjs'
import { download, startHarness, until } from '../harness.mjs'

const arg = (n) => {
  const i = process.argv.indexOf(n)
  return i > 0 ? process.argv[i + 1] : undefined
}
const home = arg('--home') ?? 'D:\\verne-build\\zeus\\scratch\\s01'
const publicUrl = arg('--url')

const hashFile = (p) =>
  new Promise((resolve, reject) => {
    const h = createHash('sha256')
    createReadStream(p).on('data', (d) => h.update(d)).on('end', () => resolve(h.digest('hex'))).on('error', reject)
  })

const t0 = Date.now()
const fx = await startFixtures(0)
const h = await startHarness({ home, fresh: true })
const status = await h.ready()
console.log(`engine ready in ${((Date.now() - t0) / 1000).toFixed(1)} s, zeusd ${status.daemon.daemon_version}, credential permissions: ${status.daemon.permissions.join(',')}`)

const size = 96 * 1024 * 1024
const url = publicUrl ?? `${fx.url}/files/ubuntu-sample.iso?size=${size}&seed=11&rate=12288`
const check = await h.post('/svc/v1/links/check', { urls: [url] })
console.log('probe:', JSON.stringify({ ...check.items[0], url: undefined }))
const started = await download(h, [{ url }])
const id = started.jobs[0].job_id
console.log('job', id, started.jobs[0].state)

const samples = []
let last
await until(
  async () => {
    const jobs = await h.get('/api/v1/jobs')
    last = jobs.items.find((j) => j.id === id)
    if (last) samples.push({ t: Date.now() - t0, state: last.state, done: last.bytes_done, rate: last.rate_ewma, eta: last.eta_s })
    return last && ['completed', 'failed', 'canceled'].includes(last.state)
  },
  { timeoutMs: 240000, everyMs: 700, what: 'the download to finish' },
)
const live = samples.filter((s) => s.state === 'downloading')
console.log(`states seen: ${[...new Set(samples.map((s) => s.state))].join(' -> ')}`)
console.log(`progress samples while downloading: ${live.length}; distinct byte counts: ${new Set(live.map((s) => s.done)).size}; max rate ${Math.max(0, ...live.map((s) => s.rate ?? 0))} B/s; eta seen: ${live.some((s) => s.eta !== null)}`)
console.log('final:', JSON.stringify({ state: last.state, bytes: last.bytes_done, total: last.bytes_total, sha256: last.sha256, path: last.path }))

const detail = await h.get(`/api/v1/jobs/${id}`)
console.log('job detail probe:', JSON.stringify({ range: detail.job?.probe?.range_support, type: detail.job?.probe?.content_type, name: detail.job?.probe?.filename }), 'result:', JSON.stringify({ sha256: detail.job?.result?.sha256, blake3: detail.job?.result?.blake3, mime: detail.job?.result?.detected_mime }))

let ok = last.state === 'completed'
if (!publicUrl) {
  const expect = sha256(11, size)
  const onDisk = await hashFile(last.path)
  console.log(`sha256 from zeusd ${last.sha256 === expect ? 'matches' : 'DIFFERS FROM'} the server's, file on disk ${onDisk === expect ? 'matches' : 'DIFFERS'} (${statSync(last.path).size} bytes)`)
  ok = ok && last.sha256 === expect && onDisk === expect && statSync(last.path).size === size
} else if (process.argv.includes('--sha256')) {
  // A public file whose SHA-256 was computed independently (for example with curl and sha256sum).
  const want = process.argv[process.argv.indexOf('--sha256') + 1]
  const onDisk = await hashFile(last.path)
  console.log(`independent SHA-256 ${last.sha256 === want ? 'matches' : 'DIFFERS FROM'} zeusd's, file on disk ${onDisk === want ? 'matches' : 'DIFFERS'} (${statSync(last.path).size} bytes)`)
  ok = ok && last.sha256 === want && onDisk === want
}
// The watcher puts a new file in the index within a moment of its last write.
let found = false
for (let i = 0; i < 30 && !found; i++) {
  const index = await h.get('/svc/v1/index')
  found = index.files.some((f) => !f.is_dir && f.path === last.path)
  if (!found) await new Promise((r) => setTimeout(r, 500))
}
console.log('in the file index:', found)
const files = await h.get('/api/v1/files')
console.log('in zeusd files.list:', files.items.some((f) => f.path === last.path), '; folder opens:', JSON.stringify(await h.post('/native/v1/open', { path: join(last.path, '..'), action: 'open' }).catch((e) => e.message)))
ok = ok && found

await h.stop()
await fx.close()
console.log(ok ? 'SCENARIO 1: PASSED' : 'SCENARIO 1: FAILED')
process.exit(ok ? 0 : 1)

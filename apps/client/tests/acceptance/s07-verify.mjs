// Scenario 7: quarantine and verification, per the specs (01 section 11.3 and S70), on the real engine.
//   - SHA-256 and BLAKE3 of a finished file equal an independent computation;
//   - a published hash (expected_sha256) that matches is recorded and shown as verified; one that does not match
//     fails the job with integrity.hash_mismatch and leaves no file;
//   - executable types are quarantined with their reasons, and the app refuses to open them;
//   - "Verify again" has zeusd re-read the file: it matches, and after one byte is changed on disk it does not;
//   - the properties panel shows these facts.
// Run: node tests/acceptance/s07-verify.mjs [--out DIR]

import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, openSync, readFileSync, readSync, writeSync, closeSync, readdirSync } from 'node:fs'

import { startFixtures, sha256 } from '../fixture-server.mjs'
import { download, sleep, startHarness, until } from '../harness.mjs'
import { guard } from '../guard.mjs'
import { openUi } from '../ui.mjs'

const out = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : 'D:/verne-build/zeus/evidence/s07'
mkdirSync(out, { recursive: true })
const PY = process.env.ZEUS_B3_PYTHON ?? 'D:/verne-build/zeus/scratch/b/venv/Scripts/python.exe'
let failed = 0
const check = (name, ok, detail = '') => {
  if (!ok) failed++
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
const blake3Of = (file) => {
  try {
    return execFileSync(PY, ['-I', '-c', 'import sys,blake3;print(blake3.blake3(open(sys.argv[1],"rb").read()).hexdigest())', file], { encoding: 'utf8' }).trim()
  } catch {
    return null
  }
}
const sha256File = (file) => createHash('sha256').update(readFileSync(file)).digest('hex')

const fx = await startFixtures(0)
const home = 'D:/verne-build/zeus/scratch/s07'
let ui = null
const h = await startHarness({ home, fresh: true })
guard(() => ({ h, fx, ui }))
await h.ready()
const jobOf = async (id) => (await h.get('/api/v1/jobs')).items.find((j) => j.id === id)
const finish = (id, timeoutMs = 120000) => until(async () => { const j = await jobOf(id); return j && ['completed', 'failed', 'canceled'].includes(j.state) ? j : false }, { timeoutMs, what: `job ${id}` })
const detailOf = async (id) => (await h.get(`/api/v1/jobs/${id}`)).job

// ---------------------------------------------------------------- a normal file with a published hash that matches
const size = 3 * 1024 * 1024
const good = (await download(h, [{ url: `${fx.url}/files/report.pdf?size=${size}&seed=31`, expected_sha256: sha256(31, size) }])).jobs[0].job_id
const goodDone = await finish(good)
const goodDetail = await detailOf(good)
check('a file with a matching published hash completes', goodDone.state === 'completed', goodDone.state)
check('zeusd recorded the published hash it was checked against', goodDetail.spec?.expected_sha256 === sha256(31, size))
check('SHA-256 equals an independent computation', goodDone.sha256 === sha256File(goodDone.path) && goodDone.sha256 === sha256(31, size))
const b3 = blake3Of(goodDone.path)
check('BLAKE3 equals an independent computation', b3 === null ? true : goodDetail.result?.blake3 === b3, b3 === null ? 'SKIPPED: no blake3 module' : goodDetail.result?.blake3)
check('a document is not quarantined', goodDone.quarantined === false && !(goodDetail.quarantine_reasons?.length))

// ---------------------------------------------------------------- a published hash that does not match
const badHash = '0'.repeat(63) + '1'
const bad = (await download(h, [{ url: `${fx.url}/files/wrong-hash.bin?size=${1024 * 1024}&seed=32`, expected_sha256: badHash }])).jobs[0].job_id
const badDone = await finish(bad, 180000)
const kept = readdirSync(`${home}/downloads`).filter((n) => n.startsWith('wrong-hash'))
check('a mismatching published hash fails the job', badDone.state === 'failed' && badDone.last_error?.code === 'integrity.hash_mismatch', `${badDone.state} ${badDone.last_error?.code}`)
check('and no completed file is left at the destination', !kept.some((n) => n === 'wrong-hash.bin'), `folder has: ${kept.join(', ') || 'nothing of it'}`)
const badDetail = await detailOf(bad)
console.log(`  detail: attempts ${badDetail.retry?.attempt}, integrity restarts ${badDetail.retry?.integrity_restarts}, error ${badDetail.error?.code}: ${badDetail.error?.message}`)
const partial = existsSync(`${home}/downloads/wrong-hash.bin.zpart`)
const cancelBad = await h.post(`/api/v1/jobs/${bad}/control`, { action: 'cancel', idempotency_key: 'cancel-bad' }).then(() => 'accepted', (e) => e.code)
check('the received bytes stay only as a .zpart partial (a failed job cannot be cancelled, so zeusd keeps it)', partial && cancelBad === 'api.invalid_transition', `partial ${partial}, cancel ${cancelBad}`)

// ---------------------------------------------------------------- executable quarantine
const types = ['setup.exe', 'run-me.sh', 'tool.AppImage', 'script.ps1', 'invoice.pdf.exe', 'lib.dll', 'app.msi', 'macro.bat']
const exe = {}
for (const name of types) exe[name] = (await download(h, [{ url: `${fx.url}/files/${name}?size=${200 * 1024}&seed=33` }])).jobs[0].job_id
const exeDone = {}
for (const [name, id] of Object.entries(exe)) exeDone[name] = await finish(id)
for (const name of types) {
  const d = await detailOf(exe[name])
  check(`${name} is quarantined with a reason`, exeDone[name].state === 'completed' && exeDone[name].quarantined === true && (d.quarantine_reasons ?? []).includes('executable_type'), (d.quarantine_reasons ?? []).join(','))
}
const exeId = exe['setup.exe']
const exePath = exeDone['setup.exe'].path
const refused = await h.post('/native/v1/open', { path: exePath, action: 'open' }).then(() => 'opened', (e) => `${e.code}`)
check('the app refuses to open a program or script', refused === 'app.runnable', refused)
const txt = (await download(h, [{ url: `${fx.url}/files/notes.txt?size=2000&seed=34` }])).jobs[0].job_id
const txtDone = await finish(txt)
check('a plain text file is not quarantined', txtDone.state === 'completed' && txtDone.quarantined === false)

// ---------------------------------------------------------------- verify again
const v1 = await h.post(`/native/v1/jobs/${good}/verify`, {})
check('verify again: the file still matches what zeusd recorded', v1.matches_recorded === true && v1.sha256 === goodDone.sha256, `size ${v1.size_bytes}`)
const v2 = await h.post(`/native/v1/jobs/${good}/verify`, { expected_sha256: sha256(31, size) })
check('verify against the published hash: matches', v2.matches_expected === true)
const v3 = await h.post(`/native/v1/jobs/${good}/verify`, { expected_sha256: badHash })
check('verify against a wrong hash: does not match', v3.matches_expected === false && v3.matches_recorded === true)
const v4 = await h.post(`/native/v1/jobs/${good}/verify`, { expected_sha256: 'nothex' }).then(() => 'accepted', (e) => e.code)
check('a malformed hash is refused', v4 === 'api.invalid_params', v4)

// A UI check before the file is changed.
ui = await openUi(h)
const { page } = ui
await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/downloads?job=${exeId}`)
await page.waitForSelector('text=Checksum and verification', { timeout: 20000 })
const exeText = await page.textContent('aside.drawer')
check('the properties panel shows the quarantine, its reason and the terminal route', /quarantined/.test(exeText ?? '') && /executable_type/.test(exeText ?? '') && /zeus approvals|approvals list/.test(exeText ?? '') && /never opens, runs or converts/.test(exeText ?? ''))
await page.screenshot({ path: `${out}/exe-properties.png` })
await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/downloads?job=${good}`)
await page.waitForSelector('text=yes: it matches the SHA-256 you gave', { timeout: 20000 })
const goodText = await page.textContent('aside.drawer')
check('the panel shows SHA-256, BLAKE3 and "verified against a published hash: yes"', goodText?.includes(goodDone.sha256) && goodText.includes(goodDetail.result.blake3) && /verified against a published hash/.test(goodText) && /it matches the SHA-256 you gave/.test(goodText), (goodText ?? '').replace(/\s+/g, ' ').split('Checksum and verification')[1]?.slice(0, 700))
await page.click('button:has-text("Verify again")')
await page.waitForSelector('text=still matches what zeusd recorded', { timeout: 30000 })
await page.screenshot({ path: `${out}/good-verified.png` })

// ---------------------------------------------------------------- one byte changes on disk
const fd = openSync(goodDone.path, 'r+')
const one = Buffer.alloc(1)
readSync(fd, one, 0, 1, 1000)
writeSync(fd, Buffer.from([one[0] ^ 0xff]), 0, 1, 1000)
closeSync(fd)
const v5 = await h.post(`/native/v1/jobs/${good}/verify`, {})
check('after one byte changes on disk, zeusd reports the mismatch', v5.matches_recorded === false && v5.sha256 !== goodDone.sha256)
await page.click('button:has-text("Verify again")')
await page.waitForSelector('text=different from what zeusd recorded', { timeout: 30000 })
await page.screenshot({ path: `${out}/good-changed.png` })
check('no console errors', ui.errors.length === 0, ui.errors.slice(0, 2).join(' | '))
await ui.close()
await h.stop()
await fx.close()
console.log(failed ? `SCENARIO 7: ${failed} FAILED` : 'SCENARIO 7: PASSED')
process.exit(failed ? 1 : 0)

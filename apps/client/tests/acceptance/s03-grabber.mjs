// Scenario 3: the link grabber with many URLs (mixed good and bad), a reviewed batch start, Add URL, categories and
// save-to rules by file type, and duplicate handling, in the real interface against the real engine.
// Run: node tests/acceptance/s03-grabber.mjs [--out DIR]

import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

import { startFixtures, sha256 } from '../fixture-server.mjs'
import { download, sleep, startHarness, until } from '../harness.mjs'
import { guard } from '../guard.mjs'
import { openUi } from '../ui.mjs'

const out = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : 'D:/verne-build/zeus/evidence/s03'
mkdirSync(out, { recursive: true })
let failed = 0
const check = (name, ok, detail = '') => {
  if (!ok) failed++
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
const fileSha = (p) => createHash('sha256').update(readFileSync(p)).digest('hex')

const fx = await startFixtures(0)
const home = 'D:/verne-build/zeus/scratch/s03'
let h = await startHarness({ home, fresh: true })
let ui = null
guard(() => ({ h, fx, ui }))
await h.ready()
const session = await h.post('/api/v1/session')
const root = session.root_label
const sep = session.sep
const jobs = async () => (await h.get('/api/v1/jobs')).items
const byName = async (name) => (await jobs()).filter((j) => j.filename === name)
const finishAll = (n, timeoutMs = 180000) =>
  until(async () => {
    const all = await jobs()
    const done = all.filter((j) => ['completed', 'failed', 'canceled'].includes(j.state))
    return done.length >= n ? all : false
  }, { timeoutMs, what: `${n} jobs to finish` })

ui = await openUi(h)
let page = ui.page
const open = async (hash) => {
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/${hash}`)
  await page.waitForSelector('.statusbar', { timeout: 30000 })
  await page.waitForTimeout(600)
}

// ---------------------------------------------------------------- A: many links, mixed good and bad
const u = (path) => `${fx.url}${path}`
const lines = [
  u('/files/annual-report.pdf?size=300000&seed=41'),
  u('/files/jazz.mp3?size=500000&seed=42'),
  u('/files/backup.zip?size=700000&seed=43'),
  u('/files/photo.jpg?size=100000&seed=44'),
  u('/files/clip.mp4?size=900000&seed=45'),
  u('/files/setup.exe?size=200000&seed=46'),
  u('/norange/plain.bin?size=300000&seed=47'),
  u('/redirect/2/moved.zip?size=250000&seed=48'),
  u('/cd/anything?size=120000&seed=49&as=Server%20Named.pdf'),
  u('/missing/gone.zip'),
  u('/expired/old.zip'),
  u('/gone/removed.pdf'),
  u('/busy/maybe.bin?size=1000&seed=1&fails=1000&after=1'),
  'ftp://files.example.org/legacy.tar',
  'magnet:?xt=urn:btih:0000',
  u('/files/annual-report.pdf?size=300000&seed=41'),
]
await open('grabber')
await page.fill('textarea[aria-label="Links or a page to grab from"]', lines.join('\n'))
await page.click('button:has-text("Add links")')
await page.waitForSelector('table.glinks', { timeout: 10000 })
const intakeText = await page.textContent('.intake')
check('13 links are taken; the ftp and magnet addresses are refused; the repeated line is dropped', (await page.locator('table.glinks tbody tr[aria-selected]').count()) === 13 && /1 exact duplicate/.test(intakeText ?? '') && /magnet|ftp/i.test(intakeText ?? ''), intakeText?.replace(/\s+/g, ' ').slice(0, 160))
await page.click('button:has-text("Check online status")')
await until(async () => (await page.locator('table.glinks .chip:has-text("checking")').count()) === 0 && (await page.locator('table.glinks .chip:has-text("not checked")').count()) === 0, { timeoutMs: 90000, everyMs: 500, what: 'the online check' })
const offline = await page.locator('table.glinks tr[aria-selected="false"]').count()
check('the four bad links are offline and deselected', (await page.locator('table.glinks .chip:has-text("offline")').count()) === 4 && offline === 4, `${offline} deselected`)
const notes = (await page.locator('tr.gnote').allTextContents()).join(' | ')
check('each bad link says why in words (403 expired, 404, 410, 503)', /expired/.test(notes) && /no such file \(404\)/.test(notes) && /removed from the server \(410\)/.test(notes) && /busy \(503\)/.test(notes), notes.slice(0, 200))
check('the server-given name is offered, with its category', (await page.locator('input[aria-label="File name"]').evaluateAll((els) => els.map((e) => e.value))).includes('Server Named.pdf') && /the server names it Server Named\.pdf/.test(notes))
check('a server without byte ranges is called out', /no byte ranges/.test(notes))
await page.screenshot({ path: `${out}/01-checked.png` })
await page.click('button:has-text("Preview downloads")')
await page.waitForSelector('section.review', { timeout: 20000 })
await page.waitForSelector('text=zeusd accepted the dry run', { timeout: 30000 })
const review = await page.locator('section.review tbody tr').evaluateAll((rows) => rows.map((r) => [...r.querySelectorAll('td')].map((c) => c.textContent?.trim())))
const folderOf = (name) => review.find((r) => r[0] === name)?.[1] ?? ''
const sub = (name, want) => folderOf(name).endsWith(`${sep}${want}`) || (want === '' && folderOf(name) === root)
check('the review puts each type in its category folder', sub('annual-report.pdf', 'Documents') && sub('jazz.mp3', 'Music') && sub('backup.zip', 'Compressed') && sub('photo.jpg', 'Pictures') && sub('clip.mp4', 'Video') && sub('setup.exe', 'Programs') && sub('Server Named.pdf', 'Documents') && sub('plain.bin', ''), review.map((r) => `${r[0]} -> ${r[1]?.replace(root, '')}`).join('; '))
await page.screenshot({ path: `${out}/02-review.png` })
await page.click('button:has-text("Start reviewed downloads")')
await page.waitForSelector('text=Started 9 downloads', { timeout: 20000 })
await page.screenshot({ path: `${out}/03-started.png` })
const all = await finishAll(9)
const ok9 = all.filter((j) => j.state === 'completed')
check('nine downloads complete, one reviewed batch', ok9.length === 9, `${ok9.length} completed, states ${all.map((j) => j.state).join(',')}`)
const seeds = { 'annual-report.pdf': [41, 300000], 'jazz.mp3': [42, 500000], 'backup.zip': [43, 700000], 'photo.jpg': [44, 100000], 'clip.mp4': [45, 900000], 'setup.exe': [46, 200000], 'plain.bin': [47, 300000], 'moved.zip': [48, 250000], 'Server Named.pdf': [49, 120000] }
let hashesOk = true
for (const j of ok9) {
  const s = seeds[j.filename]
  if (!s || j.sha256 !== sha256(s[0], s[1]) || fileSha(j.path) !== j.sha256) hashesOk = false
}
check('every file has the right bytes (SHA-256 equals the server\'s)', hashesOk)
const dirs = Object.fromEntries(ok9.map((j) => [j.filename, j.path.replace(root, '').split(sep).slice(1, -1).join(sep)]))
check('files landed in their category folders on disk', dirs['annual-report.pdf'] === 'Documents' && dirs['jazz.mp3'] === 'Music' && dirs['backup.zip'] === 'Compressed' && dirs['moved.zip'] === 'Compressed' && dirs['photo.jpg'] === 'Pictures' && dirs['clip.mp4'] === 'Video' && dirs['setup.exe'] === 'Programs' && dirs['plain.bin'] === '', JSON.stringify(dirs))
check('the program is quarantined', ok9.find((j) => j.filename === 'setup.exe')?.quarantined === true)

// ---------------------------------------------------------------- B: duplicates
await sleep(3000) // the page reads zeusd's job list every second or two; let it see that the nine are completed
await open('grabber')
if (await page.locator('button:has-text("Remove offline"):not([disabled])').count()) await page.click('button:has-text("Remove offline")')
await page.fill('textarea[aria-label="Links or a page to grab from"]', u('/files/annual-report.pdf?size=300000&seed=41'))
await page.click('button:has-text("Add links")')
await page.waitForSelector('table.glinks', { timeout: 10000 })
const dupeText = await page.textContent('tr.gnote')
check('a file that is already downloaded is flagged, and skipped by default', /already downloaded/.test(dupeText ?? '') && (await page.locator('table.glinks tr[aria-selected="false"]').count()) === 1, dupeText?.replace(/\s+/g, ' '))
await page.click('input[aria-label^="Select annual-report.pdf"]')
await page.click('button:has-text("Preview downloads")')
await page.waitForSelector('text=set to be skipped', { timeout: 10000 })
check('previewing only a skipped duplicate says nothing would be created', true)
await page.selectOption('select[aria-label^="What to do with the duplicate"]', 'again')
await page.click('button:has-text("Preview downloads")')
await page.waitForSelector('text=zeusd accepted the dry run', { timeout: 30000 })
const again = await page.locator('section.review tbody tr').first().textContent()
check('"Download again" says it will be a new copy before anything happens', /downloaded again/.test(again ?? ''), again?.replace(/\s+/g, ' '))
await page.click('button:has-text("Start reviewed downloads")')
await until(async () => (await jobs()).some((j) => (j.path ?? '').endsWith('annual-report (1).pdf') && j.state === 'completed'), { timeoutMs: 60000, what: 'the second copy' })
const docs = join(root, 'Documents')
check('both copies are on disk with the right bytes', existsSync(join(docs, 'annual-report.pdf')) && existsSync(join(docs, 'annual-report (1).pdf')) && fileSha(join(docs, 'annual-report (1).pdf')) === sha256(41, 300000))
// Replace: first damage the original, then replace it.
writeFileSync(join(docs, 'annual-report.pdf'), Buffer.from('damaged'))
await open('grabber')
await page.fill('textarea[aria-label="Links or a page to grab from"]', u('/files/annual-report.pdf?size=300000&seed=41'))
await page.click('button:has-text("Add links")')
await page.waitForSelector('table.glinks', { timeout: 10000 })
await page.selectOption('select[aria-label^="What to do with the duplicate"]', 'replace')
await page.click('button:has-text("Preview downloads")')
await page.waitForSelector('text=zeusd accepted the dry run', { timeout: 30000 })
const repl = await page.locator('section.review tbody tr').first().textContent()
check('"Replace" says it replaces the file on disk', /replaces the file on disk/.test(repl ?? ''), repl?.replace(/\s+/g, ' '))
await page.click('button:has-text("Start reviewed downloads")')
await until(async () => { try { return fileSha(join(docs, 'annual-report.pdf')) === sha256(41, 300000) } catch { return false } }, { timeoutMs: 60000, what: 'the replaced file' }).catch(async (e) => {
  console.log('DEBUG toasts:', JSON.stringify(await page.locator('[role=status], .toast, .toasts').allInnerTexts().catch(() => [])))
  console.log('DEBUG jobs:', JSON.stringify((await jobs()).map((j) => [j.filename, j.state, j.path])))
  throw e
})
check('Replace put the right bytes back, in the same place', true)

// ---------------------------------------------------------------- C: zeusd's overwrite policies, through the batch API
const dn = (seed) => ({ url: u(`/files/dup-name.pdf?size=4000&seed=${seed}`) })
const first = (await download(h, [dn(51)])).jobs[0].job_id
await until(async () => (await jobs()).find((j) => j.id === first)?.state === 'completed', { what: 'dup-name 1' })
const target = join(root, 'dup-name.pdf')
const secondFail = await download(h, [{ ...dn(52), overwrite: 'fail' }]).then((r) => r.jobs[0], (e) => ({ error_code: e.code }))
await sleep(4000)
const failJob = secondFail.job_id ? (await jobs()).find((j) => j.id === secondFail.job_id) : null
check('policy "fail": the existing file is not touched', fileSha(target) === sha256(51, 4000), failJob ? `job ${failJob.state} ${failJob.last_error?.code ?? ''}` : `refused ${secondFail.error_code}`)
check('policy "fail": the second download is refused or fails', !failJob || failJob.state === 'failed', failJob ? failJob.state : 'refused at enqueue')
const third = (await download(h, [{ ...dn(53), overwrite: 'rename' }])).jobs[0].job_id
await until(async () => (await jobs()).find((j) => j.id === third)?.state === 'completed', { what: 'rename' })
check('policy "rename": a second file with (1) appears, the first stays', existsSync(join(root, 'dup-name (1).pdf')) && fileSha(target) === sha256(51, 4000) && fileSha(join(root, 'dup-name (1).pdf')) === sha256(53, 4000))
// zeusd's own "replace" moves the old file to its trash only after a human-approved destructive operation; this app's
// credential holds no such authority, so the daemon refuses it and the file is untouched (the app replaces by deleting
// the old file at the person's click, tested above).
const fourth = await download(h, [{ ...dn(54), overwrite: 'replace' }]).then((r) => r.jobs[0], (e) => ({ error_code: e.code, error_message: e.message }))
await sleep(2000)
check('policy "replace": zeusd refuses it for this credential (it needs a human-approved destructive operation) and the file is untouched', !fourth.job_id && fileSha(target) === sha256(51, 4000), `${fourth.error_code ?? fourth.state}`)

// ---------------------------------------------------------------- D: Add URL, save-to rules, persistence
await open('settings/saveto')
const films = join(root, 'Films')
await page.fill('input[aria-label="Folder for Video"]', films)
await page.press('input[aria-label="Folder for Video"]', 'Tab')
await page.fill('input[aria-label="Folder for Music"]', '/outside/not/allowed')
await page.press('input[aria-label="Folder for Music"]', 'Tab')
check('a folder outside the download folders is called out', /outside the download folders|full folder path/.test((await page.textContent('body')) ?? ''))
await page.fill('input[aria-label="Folder for Music"]', '')
await page.press('input[aria-label="Folder for Music"]', 'Tab')
await page.screenshot({ path: `${out}/04-saveto.png` })
async function addUrlFolder(url) {
  await page.click('button:has-text("Add URL") >> nth=0')
  await page.fill('input.mono', url)
  await page.click('button:has-text("Check")')
  await page.waitForSelector('.probe', { timeout: 20000 })
  const folder = await page.inputValue('label:has-text("Folder") input')
  return folder
}
await open('downloads')
const f1 = await addUrlFolder(u('/files/film.mp4?size=300000&seed=61'))
check('Add URL follows the category folder rule for video', f1 === films, f1)
await page.screenshot({ path: `${out}/05-add-url-films.png` })
await page.click('button:has-text("Start download")')
await until(async () => (await byName('film.mp4')).some((j) => j.state === 'completed'), { timeoutMs: 60000, what: 'film.mp4' })
check('the video went to the folder of its own', (await byName('film.mp4'))[0].path === join(films, 'film.mp4').replace(/\//g, sep))
// "Ask where to save each download"
await open('settings/saveto')
await page.click('[aria-label="Ask each time"]')
await open('downloads')
const f2 = await addUrlFolder(u('/files/ask.pdf?size=10000&seed=62'))
const startDisabled = await page.locator('button:has-text("Start download")').isDisabled()
check('"ask each time" holds the download until the folder is confirmed', startDisabled && (await page.locator('text=Save here').count()) > 0, f2)
await page.click('button:has-text("Save here")')
check('and then lets it start', !(await page.locator('button:has-text("Start download")').isDisabled()))
await page.keyboard.press('Escape')
await open('settings/saveto')
await page.click('[aria-label="Ask each time"]')
// "Remember the last folder": a folder typed once comes back for that type of file.
await open('downloads')
await addUrlFolder(u('/files/tune1.mp3?size=5000&seed=64'))
const tunes = join(root, 'Tunes')
await page.fill('label:has-text("Folder") input', tunes)
await page.click('button:has-text("Start download")')
await until(async () => (await byName('tune1.mp3')).some((j) => j.state === 'completed'), { timeoutMs: 60000, what: 'tune1.mp3' })
await open('downloads')
const f5 = await addUrlFolder(u('/files/tune2.mp3?size=5000&seed=65'))
check('"remember the last folder" brings a typed folder back for that type', f5 === tunes, f5)
await page.keyboard.press('Escape')
// Duplicate inside Add URL
await open('downloads')
await page.click('button:has-text("Add URL") >> nth=0')
await page.fill('input.mono', u('/files/jazz.mp3?size=500000&seed=42'))
await page.click('button:has-text("Check")')
await page.waitForSelector('.probe', { timeout: 20000 })
const dupeNotice = await page.textContent('.notice.warn')
check('Add URL flags a duplicate and holds Start until a choice is made', /already downloaded/.test(dupeNotice ?? '') && (await page.locator('button:has-text("Start download")').isDisabled()))
await page.selectOption('select[aria-label="What to do with the duplicate"]', 'again')
check('"Download again" enables it', !(await page.locator('button:has-text("Start download")').isDisabled()))
await page.screenshot({ path: `${out}/06-add-url-duplicate.png` })
await page.keyboard.press('Escape')

// Persistence across a restart of the app's engine and window.
await sleep(1600)
await ui.close()
await h.stop()
h = await startHarness({ home })
await h.ready()
ui = await openUi(h)
page = ui.page
await open('settings/saveto')
const persisted = await page.inputValue('input[aria-label="Folder for Video"]')
check('the folder rule for video survived a restart', persisted === films, persisted)
await open('downloads')
const f3 = await addUrlFolder(u('/files/film2.mp4?size=1000&seed=63'))
check('and still decides where video goes', f3 === films, f3)
check('no console errors', ui.errors.length === 0, ui.errors.slice(0, 2).join(' | '))
await ui.close()
await h.stop()
await fx.close()
console.log(failed ? `SCENARIO 3: ${failed} FAILED` : 'SCENARIO 3: PASSED')
process.exit(failed ? 1 : 0)

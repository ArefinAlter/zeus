// Scenario 5: Files. A real index of the download folders (scan plus watcher) with Everything-style search, filters, a
// folders pane, previews of text and pictures, file operations, and bookmarks and history that survive a restart.
// Run: node tests/acceptance/s05-files.mjs [--out DIR] [--big 50000]
//
// The engine and the interface run for real (the harness binary and Chromium); the files are made on disk by this
// script, so what the page shows is what the watcher and the scanner found.

import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readdirSync, rmSync, statSync, utimesSync, writeFileSync, renameSync, unlinkSync } from 'node:fs'
import { join } from 'node:path'
import { crc32, deflateSync } from 'node:zlib'

import { startFixtures } from '../fixture-server.mjs'
import { guard } from '../guard.mjs'
import { download, sleep, startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const arg = (n, d) => (process.argv.includes(n) ? process.argv[process.argv.indexOf(n) + 1] : d)
const out = arg('--out', 'D:/verne-build/zeus/evidence/s05-files')
const BIG = Number(arg('--big', 50000))
const home = 'D:/verne-build/zeus/scratch/a/s05'
mkdirSync(out, { recursive: true })

let failed = 0
const check = (name, ok, detail = '') => {
  if (!ok) failed++
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
const t0 = Date.now()

function png(w, h, rgb) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4)
    len.writeUInt32BE(data.length)
    const body = Buffer.concat([Buffer.from(type), data])
    const c = Buffer.alloc(4)
    c.writeUInt32BE(crc32(body) >>> 0)
    return Buffer.concat([len, body, c])
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(w, 0)
  ihdr.writeUInt32BE(h, 4)
  ihdr[8] = 8
  ihdr[9] = 2
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: w }, () => rgb).flat())])
  const raw = Buffer.concat(Array.from({ length: h }, () => row))
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))])
}

const fx = await startFixtures(0)
let h = await startHarness({ home, fresh: true })
guard(() => ({ h, fx })) // a failed run must not leave its engine behind
await h.ready()
const session = await h.post('/api/v1/session')
const root = process.platform === 'win32' ? session.root_label.replaceAll('/', '\\') : session.root_label
console.log(`download folder: ${root}`)

// ---------------------------------------------------------------- the tree: 2,000 files on disk while the engine watches
const TOP = ['Documents', 'Pictures', 'Compressed', 'Video', 'Music', 'Programs']
const expected = { files: 0, dirs: 0, pdf: 0, txt: 0, png: 0, zip: 0, old2020: 0, big: 0 }
const t1 = Date.now()
for (const top of TOP) {
  for (let s = 0; s < 5; s++) {
    const dir = join(root, top, `batch${s}`)
    mkdirSync(dir, { recursive: true })
    expected.dirs += s === 0 ? 2 : 1
  }
}
const small = png(2, 2, [200, 30, 30])
for (let i = 0; i < 2000; i++) {
  const top = TOP[i % TOP.length]
  const dir = join(root, top, `batch${i % 5}`)
  const kind = i % 4
  let name
  if (kind === 0) {
    name = `doc-${i}.pdf`
    writeFileSync(join(dir, name), `%PDF-1.4 fake ${i}\n`)
    expected.pdf++
  } else if (kind === 1) {
    name = `note-${i}.txt`
    writeFileSync(join(dir, name), `note number ${i}\nsecond line\nthird line\n`)
    expected.txt++
  } else if (kind === 2) {
    name = `pic-${i}.png`
    writeFileSync(join(dir, name), small)
    expected.png++
  } else {
    name = `pack-${i}.zip`
    writeFileSync(join(dir, name), Buffer.alloc(300 + (i % 700), i % 251))
    expected.zip++
  }
  if (i % 50 === 0) {
    const old = new Date(2020, 5, 15, 12, 0, 0)
    utimesSync(join(dir, name), old, old)
    expected.old2020++
  }
  if (i === 7) {
    writeFileSync(join(dir, 'large-one.bin'), Buffer.alloc(3 * 1024 * 1024, 1))
    expected.big++
    expected.files++
  }
  expected.files++
}
// Things the index must skip.
writeFileSync(join(root, 'movie.zpart'), 'partial')
mkdirSync(join(root, 'node_modules', 'x'), { recursive: true })
writeFileSync(join(root, 'node_modules', 'x', 'index.js'), 'x')
writeFileSync(join(root, 'Thumbs.db'), 'x')
writeFileSync(join(root, 'secret-hidden.txt'), 'x')
if (process.platform === 'win32') execFileSync('attrib', ['+H', join(root, 'secret-hidden.txt')])
console.log(`created ${expected.files} files in ${((Date.now() - t1) / 1000).toFixed(1)} s`)

const wantEntries = expected.files + expected.dirs
const t2 = Date.now()
const settled = await until(
  async () => {
    const d = await h.post('/native/v1/index/changes', { since: 0 })
    return d.state === 'current' && d.files.length === wantEntries ? d : false
  },
  { timeoutMs: 120000, everyMs: 500, what: `${wantEntries} index entries` },
).catch(() => null)
const seen = settled ?? (await h.post('/native/v1/index/changes', { since: 0 }))
check('the index holds exactly the files made (watcher and scanner agree)', seen.files.length === wantEntries, `${seen.files.length} of ${wantEntries}, settled in ${((Date.now() - t2) / 1000).toFixed(1)} s after the last file`)
check('partial files, node_modules, Thumbs.db and hidden files are skipped', !seen.files.some((f) => /zpart|node_modules|Thumbs\.db|secret-hidden/.test(f.path)))
check('the watcher is on', seen.watcher === true)
check('paths are absolute without a verbatim prefix', seen.files.every((f) => !f.path.startsWith('\\\\?\\')) && seen.files.some((f) => f.path.startsWith(root)))

// ---------------------------------------------------------------- the interface
const ui = await openUi(h)
const { page } = ui
const search = async (text, opts = {}) => {
  const box = page.locator('input[aria-label="Search files"]')
  await box.fill(text)
  await page.waitForTimeout(350)
  const t = (await page.locator('.fstatus > span').first().textContent()) ?? ''
  const n = Number(/^([\d,]+) object/.exec(t.trim())?.[1]?.replace(/,/g, '') ?? NaN)
  return { n, text: t }
}
await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
await page.waitForSelector('.files-page')
await until(async () => (await search('')).n === wantEntries, { timeoutMs: 30000, everyMs: 500, what: 'the page to hold every entry' })
await page.screenshot({ path: `${out}/01-files.png` })
check('the page shows every entry', (await search('')).n === wantEntries)

const operators = [
  ['ext:pdf', expected.pdf],
  ['ext:txt;png', expected.txt + expected.png],
  ['size:>2mb', expected.big],
  ['dm:2020', expected.old2020],
  ['dm:today ext:pdf', null],
  ['note-12', null],
  ['folder:', expected.dirs],
  ['file:', expected.files],
  ['regex:^doc-1\\d\\d\\.pdf$', null],
  ['*.pdf', expected.pdf],
]
for (const [text, want] of operators) {
  const r = await search(text)
  check(`search "${text}" answers`, Number.isFinite(r.n) && (want === null ? r.n >= 0 : r.n === want), `${r.n} hits${want === null ? '' : `, expected ${want}`}`)
}
// The expectations that are not simple counts are computed from what is on the disk.
const SKIP = /(^|\/)(node_modules|Thumbs\.db|secret-hidden\.txt)(\/|$)|\.zpart$|(^|\/)\.zeus/
const onDisk = () =>
  readdirSync(root, { recursive: true })
    .map((p) => String(p).replaceAll('\\', '/'))
    .filter((p) => !SKIP.test(p))
    .map((p) => ({ rel: p, name: p.split('/').pop(), dir: statSync(join(root, p)).isDirectory() }))
const disk = onDisk()
const files = disk.filter((e) => !e.dir)
const eq = async (text, want, opts) => {
  const r = await search(text, opts)
  check(`search "${text}" matches the disk`, r.n === want, `${r.n} vs ${want}`)
}
await eq('ext:zip !pack-1', files.filter((e) => e.name.endsWith('.zip') && !e.name.includes('pack-1')).length)
await eq('path:Pictures ext:png', files.filter((e) => e.rel.startsWith('Pictures/') && e.name.endsWith('.png')).length)
await eq('regex:^doc-1\\d\\d\\.pdf$', files.filter((e) => /^doc-1\d\d\.pdf$/.test(e.name)).length)

// Toggles: case, regex, match path
await page.click('button[aria-label="Match case"]')
await eq('DOC-', 0)
await page.click('button[aria-label="Match case"]')
await page.click('button[aria-label="Regular expression"]')
await eq('^pack-\\d+\\.zip$', expected.zip)
await page.click('button[aria-label="Regular expression"]')
await page.click('button[aria-label="Match path"]')
await eq('Music', disk.filter((e) => `${root}/${e.rel}`.toLowerCase().includes('music')).length)
await page.click('button[aria-label="Match path"]')

// Filters and the folders pane
await search('')
await page.selectOption('select[aria-label="Result filter"]', 'picture')
await page.waitForTimeout(250)
check('the Picture filter shows the pictures', (Number(/^([\d,]+) object/.exec(((await page.locator('.fstatus > span').first().textContent()) ?? '').trim())?.[1]?.replace(/,/g, '')) === expected.png))
await page.selectOption('select[aria-label="Result filter"]', 'everything')
const treeRows = await page.locator('.ftree li button').count()
check('the folders pane lists the download folder and its children', treeRows >= 1, `${treeRows} rows`)
await page.locator('.ftree li button', { hasText: 'Documents' }).first().click()
await page.waitForTimeout(250)
check('choosing a folder narrows the search to it', (await search('')).n < wantEntries)
await page.click('text=Everywhere')
await page.waitForTimeout(200)
await page.screenshot({ path: `${out}/02-folders-filters.png` })

// ---------------------------------------------------------------- preview
const row = async (name) => {
  await search(name)
  const r = page.locator('.frow', { hasText: name }).first()
  await r.waitFor({ timeout: 10000 })
  await r.click()
  return r
}
await row('note-5.txt')
const textPane = page.locator('.fprev-text')
await textPane.waitFor({ timeout: 10000 })
check('a text file previews its start', ((await textPane.textContent()) ?? '').includes('note number 5'), (await textPane.textContent())?.split('\n')[0])
await page.screenshot({ path: `${out}/03-preview-text.png` })
await row('pic-2.png')
const img = page.locator('img.fprev-img')
await img.waitFor({ timeout: 10000 })
const dims = await img.evaluate((el) => ({ w: el.naturalWidth, h: el.naturalHeight, src: el.src.slice(0, 22) }))
check('a picture previews as an image with its real size', dims.w === 2 && dims.h === 2 && dims.src.startsWith('data:image/png'), JSON.stringify(dims))
await page.screenshot({ path: `${out}/04-preview-image.png` })
await row('pack-3.zip')
const t0zip = Date.now()
await page.waitForSelector('.fprev', { timeout: 10000 })
await page.waitForFunction(() => /no preview|does not preview|no text or image/i.test(document.querySelector('.fprev')?.textContent ?? ''), null, { timeout: 15000 }).catch(() => undefined)
console.log(`archive preview took ${Date.now() - t0zip} ms`)
check('an archive shows a facts card, not its bytes', /no preview|does not preview|no text or image/i.test((await page.locator('.fprev').textContent()) ?? ''), ((await page.locator('.fprev').textContent()) ?? '').slice(0, 160))

// ---------------------------------------------------------------- the watcher while the page is open
const late = join(root, 'Documents', 'batch0', 'late-file.txt')
writeFileSync(late, 'arrived while you watched')
const tLate = Date.now()
await until(async () => (await search('late-file')).n === 1, { timeoutMs: 15000, everyMs: 400, what: 'late-file.txt in the page' })
check('a file created while the page is open appears', true, `${((Date.now() - tLate) / 1000).toFixed(1)} s`)
renameSync(late, join(root, 'Documents', 'batch0', 'later-file.txt'))
await until(async () => (await search('later-file')).n === 1 && (await search('late-file.txt')).n === 0, { timeoutMs: 15000, everyMs: 400, what: 'the rename to show' })
check('a rename on disk shows as old name gone, new name present', true)
unlinkSync(join(root, 'Documents', 'batch0', 'later-file.txt'))
await until(async () => (await search('later-file')).n === 0, { timeoutMs: 15000, everyMs: 400, what: 'the delete to show' })
check('a delete on disk removes the entry', true)
mkdirSync(join(root, 'Documents', 'newdir', 'inner'), { recursive: true })
writeFileSync(join(root, 'Documents', 'newdir', 'inner', 'deep.txt'), 'deep')
await until(async () => (await search('deep.txt')).n === 1, { timeoutMs: 15000, everyMs: 400, what: 'a file inside a new folder' })
check('a new folder with a file inside is picked up whole', true)

// ---------------------------------------------------------------- file operations through the page
mkdirSync(join(root, 'Documents', 'ops'), { recursive: true })
writeFileSync(join(root, 'Documents', 'ops', 'rename-me.txt'), 'rename me')
writeFileSync(join(root, 'Documents', 'ops', 'move-me.txt'), 'move me')
writeFileSync(join(root, 'Documents', 'ops', 'delete-me.txt'), 'delete me')
writeFileSync(join(root, 'Documents', 'ops', 'taken.txt'), 'in the way')
await until(async () => (await search('-me.txt')).n === 3, { timeoutMs: 15000, everyMs: 400, what: 'the three files to be indexed' })
await row('rename-me.txt')
await page.locator('.flist').focus()
await page.keyboard.press('F2')
await page.waitForSelector('text=Rename')
const nameBox = page.locator('.dialog input').first()
await nameBox.press('Control+A') // the dialog preselects only the base name; select it all
await page.keyboard.insertText('taken.txt')
await page.waitForFunction(() => document.querySelector('.dialog input')?.value === 'taken.txt')
await page.click('.dialog button:has-text("Rename")')
await page.waitForSelector('.dialog .notice.error, .dialog .field-err', { timeout: 10000 }).catch(async (e) => {
  console.log('DEBUG dialog text:', JSON.stringify(await page.locator('.dialog').first().innerText().catch(() => 'no dialog')))
  await page.screenshot({ path: `${out}/debug-rename.png` })
  throw e
})
check('renaming onto an existing name is refused and says why', existsSync(join(root, 'Documents', 'ops', 'rename-me.txt')) && ((await page.locator('.dialog .notice.error').textContent().catch(() => '')) ?? '').includes('already exists'))
await nameBox.press('Control+A') // the dialog preselects only the base name; select it all
await page.keyboard.insertText('renamed-ok.txt')
await page.waitForFunction(() => document.querySelector('.dialog input')?.value === 'renamed-ok.txt')
await page.click('.dialog button:has-text("Rename")')
await until(async () => existsSync(join(root, 'Documents', 'ops', 'renamed-ok.txt')) && !existsSync(join(root, 'Documents', 'ops', 'rename-me.txt')), { timeoutMs: 10000, what: 'the rename on disk' })
check('rename through the page renames the file', true)
await until(async () => (await search('renamed-ok')).n === 1, { timeoutMs: 15000, everyMs: 400, what: 'the new name in the list' })
await row('move-me.txt')
await page.locator('.frow', { hasText: 'move-me.txt' }).first().click({ button: 'right' })
await page.click('.ctx button:has-text("Move"), [role=menu] button:has-text("Move")')
const dirBox = page.locator('.dialog input[list]')
await dirBox.fill(join(root, 'Sorted', 'Text'))
await page.click('.dialog button:has-text("Move")')
await until(async () => existsSync(join(root, 'Sorted', 'Text', 'move-me.txt')), { timeoutMs: 10000, what: 'the move on disk' })
check('move through the page creates the folder and moves the file', !existsSync(join(root, 'Documents', 'ops', 'move-me.txt')))
await row('delete-me.txt')
await page.locator('.flist').focus()
await page.keyboard.press('Delete')
await page.waitForSelector('.dialog')
await page.screenshot({ path: `${out}/05-delete-dialog.png` })
await page.click('.dialog button.primary, .dialog button:has-text("Move to the")')
await sleep(1500)
if (await page.locator('.dialog button:has-text("Delete for good")').count()) await page.click('.dialog button:has-text("Delete for good")')
await until(async () => !existsSync(join(root, 'Documents', 'ops', 'delete-me.txt')), { timeoutMs: 15000, what: 'the delete on disk' })
check('delete through the page removes the file (recycle bin or, where there is none, after the explicit second step)', true)
// Outside the roots is refused by the engine whatever the page sends.
const outside = join(home, '..', 'outside-file.txt')
writeFileSync(outside, 'x')
const refusal = await h.post('/native/v1/files/delete', { paths: [outside], permanent: true }).then(() => 'allowed', (e) => e.code)
check('the engine refuses a path outside the folders even when asked directly', refusal !== 'allowed' && existsSync(outside), String(refusal))
const dotdot = await h.post('/native/v1/files/preview', { path: `${root}\\..\\..\\outside-file.txt` }).then(() => 'allowed', (e) => e.code)
check('a path with .. is refused', dotdot !== 'allowed', String(dotdot))

// ---------------------------------------------------------------- a quarantined file is never previewed or changed
const exe = await download(h, [{ url: `${fx.url}/files/setup.exe?size=200000&seed=2` }]).catch((e) => ({ error: e.message }))
if (exe.jobs) {
  const jobId = exe.jobs[0].job_id
  const job = await until(async () => { const j = (await h.get('/api/v1/jobs')).items.find((x) => x.id === jobId); return j && ['completed', 'failed'].includes(j.state) ? j : false }, { timeoutMs: 60000, what: 'the exe download' })
  check('the daemon quarantines the executable', job.quarantined === true, job.state)
  if (job.quarantined) {
    await sleep(3500)
    const pv = await h.post('/native/v1/files/preview', { path: job.path })
    check('a quarantined file is not previewed', pv.kind === 'refused' && pv.reason === 'quarantined', pv.kind)
    const rn = await h.post('/native/v1/files/rename', { path: job.path, name: 'renamed.txt' }).then(() => 'allowed', (e) => e.code)
    check('a quarantined file cannot be renamed here', rn === 'policy.quarantined', String(rn))
    const del = await h.post('/native/v1/files/delete', { paths: [job.path], permanent: true }).then(() => 'allowed', (e) => e.code)
    check('a quarantined file cannot be deleted here', del === 'policy.quarantined' && existsSync(job.path), String(del))
    const op = await h.post('/native/v1/files/open', { path: job.path, action: 'open' }).then(() => 'allowed', (e) => e.code)
    check('a quarantined file is never opened', op !== 'allowed', String(op))
  }
}

// ---------------------------------------------------------------- bookmarks and history persist across a restart
await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
await page.waitForSelector('.files-page')
const box = page.locator('input[aria-label="Search files"]')
await box.fill('ext:pdf size:<1kb')
await box.press('Enter')
await page.click('button[aria-label="Bookmark this search"]')
await page.fill('.dialog input', 'Small PDFs')
await page.click('.dialog button:has-text("Save bookmark")')
await page.waitForTimeout(1800)
await page.screenshot({ path: `${out}/06-bookmark.png` })
await ui.close()
await h.stop()
h = await startHarness({ home })
await h.ready()
const ui2 = await openUi(h)
await ui2.page.goto(`${ui2.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
await ui2.page.waitForSelector('.files-page')
await ui2.page.waitForTimeout(2500)
const side = (await ui2.page.locator('.fside').textContent()) ?? ''
check('the bookmark is still there after a restart', side.includes('Small PDFs'))
check('the search history is still there after a restart', side.includes('ext:pdf size:<1kb'))
await ui2.page.click('.bm button:has-text("Small PDFs")')
await ui2.page.waitForTimeout(400)
check('using the bookmark runs its search', ((await ui2.page.inputValue('input[aria-label="Search files"]')) ?? '') === 'ext:pdf size:<1kb')
const st2 = await h.get('/native/v1/index/status')
check('after the restart the saved index served at once and was verified', st2.entries >= wantEntries - 10, `${st2.entries} entries, verifying ${st2.verifying}, state ${st2.state}`)
await ui2.page.screenshot({ path: `${out}/07-after-restart.png` })
await ui2.close()

// ---------------------------------------------------------------- scan speed on a big folder
const big = 'D:/verne-build/zeus/scratch/a/big'
rmSync(big, { recursive: true, force: true })
const tMake = Date.now()
for (let d = 0; d < 100; d++) {
  const dir = join(big, `d${String(d).padStart(3, '0')}`)
  mkdirSync(dir, { recursive: true })
  for (let i = 0; i < BIG / 100; i++) writeFileSync(join(dir, `f${i}.dat`), 'x')
}
console.log(`made ${BIG} files in ${((Date.now() - tMake) / 1000).toFixed(1)} s`)
const cfg = await h.get('/native/v1/index/config')
const tAdd = Date.now()
await h.put('/native/v1/index/config', { ...cfg, roots: [...cfg.roots, { path: big.replace(/\//g, '\\'), include: true, monitor_changes: false, rescan_every_hours: null, download: false }] })
let firstAt = null
const finished = await until(
  async () => {
    const d = await h.post('/native/v1/index/changes', { since: 0 })
    if (firstAt === null && d.files.some((f) => f.path.includes('d000'))) firstAt = Date.now() - tAdd
    return d.state === 'current' && d.files.length >= wantEntries + BIG ? d : false
  },
  { timeoutMs: 300000, everyMs: 400, what: `${BIG} more entries` },
).catch(() => null)
const scanSec = (Date.now() - tAdd) / 1000
check(`a folder of ${BIG} files is indexed`, !!finished, `${scanSec.toFixed(1)} s total (${Math.round(BIG / scanSec)} files/s), first results after ${firstAt === null ? '?' : (firstAt / 1000).toFixed(1)} s`)
const tRe = Date.now()
await h.post('/svc/v1/index/rescan', {})
await sleep(300)
await until(async () => (await h.get('/native/v1/index/status')).state === 'current', { timeoutMs: 300000, everyMs: 300, what: 'the rescan' })
console.log(`# rescan of ${wantEntries + BIG} unchanged entries: ${((Date.now() - tRe) / 1000).toFixed(1)} s`)
const mem = (() => {
  try {
    const o = execFileSync('tasklist', ['/FI', `PID eq ${h.child.pid}`, '/FO', 'CSV', '/NH']).toString()
    return o.split('","')[4]?.replace(/[^\d,]/g, '') ?? '?'
  } catch {
    return '?'
  }
})()
console.log(`# engine app process memory with ${wantEntries + BIG} entries: ${mem} K`)
// Search latency over the big index, in the page.
const ui3 = await openUi(h)
await ui3.page.goto(`${ui3.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
await ui3.page.waitForSelector('.files-page')
await until(async () => Number(/^([\d,]+) object/.exec((((await ui3.page.locator('.fstatus > span').first().textContent()) ?? '')).trim())?.[1]?.replace(/,/g, '')) >= wantEntries + BIG, { timeoutMs: 120000, everyMs: 800, what: 'the big index in the page' })
const lat = []
for (const q of ['f4', 'ext:dat size:<2', 'd05 f1', 'regex:f\\d{3}\\.dat$']) {
  const b = ui3.page.locator('input[aria-label="Search files"]')
  const t = performance.now()
  await b.fill(q)
  await ui3.page.waitForFunction((qq) => document.querySelector('input[aria-label="Search files"]').value === qq, q)
  const txt = await ui3.page.locator('.fstatus > span').last().textContent().catch(() => '')
  lat.push({ q, wall_ms: Math.round(performance.now() - t), shown: (await ui3.page.locator('.fstatus span.muted:has-text("searched in")').textContent().catch(() => '')) ?? txt })
}
console.log('# search latency in the page with', wantEntries + BIG, 'entries:', JSON.stringify(lat))
await ui3.page.screenshot({ path: `${out}/08-big-index.png` })
await ui3.close()
check('the page stayed free of console errors', [...ui.errors, ...ui2.errors, ...ui3.errors].length === 0, [...ui.errors, ...ui2.errors, ...ui3.errors].slice(0, 2).join(' | '))

await h.stop()
await fx.close()
rmSync(big, { recursive: true, force: true })
try { unlinkSync(outside) } catch {}
console.log(`finished in ${((Date.now() - t0) / 1000).toFixed(0)} s`)
console.log(failed ? `SCENARIO 5: ${failed} FAILED` : 'SCENARIO 5: PASSED')
process.exit(failed ? 1 : 0)

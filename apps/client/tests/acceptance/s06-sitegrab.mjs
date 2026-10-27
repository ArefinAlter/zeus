// Scenario 6: the site grabber against a local fixture site. Same-host crawl, depth, file-type filters, include and
// exclude rules, robots.txt (Disallow and Crawl-delay) honoured, polite pacing, cancellation, then the whole thing
// through the dialog, the plan approval held by the app, and real downloads with the right bytes.
// Run: node tests/acceptance/s06-sitegrab.mjs [--out DIR]

import { createHash } from 'node:crypto'
import { mkdirSync, readFileSync } from 'node:fs'

import { startFixtures, sha256, siteFile } from '../fixture-server.mjs'
import { sleep, startHarness, until } from '../harness.mjs'
import { guard } from '../guard.mjs'
import { openUi } from '../ui.mjs'

const out = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : 'D:/verne-build/zeus/evidence/s06'
mkdirSync(out, { recursive: true })
let failed = 0
const check = (name, ok, detail = '') => {
  if (!ok) failed++
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
const fileSha = (p) => createHash('sha256').update(readFileSync(p)).digest('hex')
const skipped = (p, reason) => p.skipped.find((s) => s.reason === reason)?.count ?? 0

const fx = await startFixtures(0)
let ui = null
const h = await startHarness({ home: 'D:/verne-build/zeus/scratch/s06', fresh: true })
guard(() => ({ h, fx, ui }))
await h.ready()
const base = `${fx.url}/site/`
const request = (over = {}) => ({ startUrl: base, depth: 2, sameHostOnly: true, types: [], include: [], exclude: [], maxFiles: 50, maxMiB: 100, delayMs: 250, ...over })
const since = (t0) => fx.log.filter((r) => r.t >= t0 && (r.path.startsWith('/site/') || r.path === '/robots.txt'))

// ---------------------------------------------------------------- the full walk
const t0 = Date.now()
const p = await h.post('/svc/v1/sitegrab/preview', request())
const seen = since(t0)
const pages = p.pages.map((x) => x.url.replace(fx.url, ''))
check('the walk ran', p.ok === true, p.error ?? `${p.pages.length} pages, ${p.files.length} files, ${p.requests} requests in ${Math.round(p.elapsedMs / 1000)} s`)
check('it read the pages within depth 2 and not the deeper one', ['/site/', '/site/guide/', '/site/blog/', '/site/loop/', '/site/guide/chapter1/', '/site/blog/post1/'].every((x) => pages.includes(x)) && !pages.includes('/site/guide/chapter1/deep/'), pages.join(' '))
const names = p.files.map((f) => f.name).sort()
check('it found exactly the allowed files', JSON.stringify(names) === JSON.stringify(['blog.zip', 'chapter1.pdf', 'data.zip', 'guide.pdf', 'notes.txt', 'post.png', 'report.pdf']), names.join(','))
check('with the sizes the server reports', p.files.every((f) => f.size === siteFile(f.name).size) && p.totalBytes === p.files.reduce((n, f) => n + siteFile(f.name).size, 0))
check('it recorded where each file was found and at what depth', p.files.find((f) => f.name === 'chapter1.pdf')?.depth === 2 && p.files.find((f) => f.name === 'report.pdf')?.depth === 0)
check('robots.txt: the private area is skipped and never requested', skipped(p, 'blocked by robots.txt') >= 1 && !seen.some((r) => r.path.startsWith('/site/private/')), `blocked ${skipped(p, 'blocked by robots.txt')}`)
check('it says what it left out and why', skipped(p, 'deeper than the depth limit') >= 1 && skipped(p, 'file on another host') === 1 && skipped(p, 'not an HTML page') === 1 && skipped(p, 'page did not answer') === 1 && skipped(p, 'duplicate') === 1, JSON.stringify(p.skipped))
check('robots.txt was read first and its crawl delay reported', seen[0]?.path === '/robots.txt' && p.robots.found && p.robots.crawl_delay_ms === 1000)
const gaps = seen.slice(1).map((r, i) => r.t - (seen[i]?.t ?? r.t))
check('requests are spaced by the site\'s Crawl-delay (at least 1 s, although 250 ms was asked)', gaps.every((g) => g >= 950), `smallest gap ${Math.min(...gaps)} ms over ${seen.length} requests`)
check('the walk names itself', seen.every((r) => /Zeus\/\S+ \(\+site grabber\)/.test(r.ua ?? '')), seen[0]?.ua)

// ---------------------------------------------------------------- filters and limits
const pdf = await h.post('/svc/v1/sitegrab/preview', request({ depth: 1, types: ['pdf'] }))
check('a file type filter keeps only that type, and depth 1 stops at the first links', JSON.stringify(pdf.files.map((f) => f.name).sort()) === JSON.stringify(['guide.pdf', 'report.pdf']) && skipped(pdf, 'file type not selected') >= 1, pdf.files.map((f) => f.name).join(','))
const rules = await h.post('/svc/v1/sitegrab/preview', request({ depth: 2, exclude: ['*/blog/*', 'notes'], include: ['/site/files/*'], types: [] }))
check('exclude and include rules apply to pages and files', !rules.files.some((f) => /blog|notes/.test(f.url)) && rules.pages.every((q) => !q.url.includes('/blog/')) && rules.files.length === 4, rules.files.map((f) => f.name).join(','))
const lim = await h.post('/svc/v1/sitegrab/preview', request({ depth: 0, maxFiles: 1 }))
check('the file limit stops the list and counts what it left out', lim.files.length === 1 && skipped(lim, 'over the file limit') >= 1)
const zero = await h.post('/svc/v1/sitegrab/preview', request({ depth: 0 }))
check('depth 0 reads only the start page', zero.pages.length === 1 && zero.files.every((f) => f.depth === 0))
const bad = await h.post('/svc/v1/sitegrab/preview', request({ startUrl: 'ftp://example.org/' }))
check('an address that is not http or https is refused with a sentence', bad.ok === false && /http or https/.test(bad.error))
const cred = await h.post('/svc/v1/sitegrab/preview', request({ startUrl: 'http://user:pw@127.0.0.1/' }))
check('an address with a password is refused', cred.ok === false && /user name or password/.test(cred.error))

// ---------------------------------------------------------------- cancellation
const slow = h.post('/svc/v1/sitegrab/preview', request({ depth: 4, delayMs: 1500, walkId: 'cancel-me' }))
await sleep(2500)
const t1 = Date.now()
const c = await h.post('/native/v1/sitegrab/cancel', { walkId: 'cancel-me' })
const cancelled = await slow
check('a walk can be cancelled and stops asking', c.found === true && cancelled.ok === false && /cancelled/.test(cancelled.error) && Date.now() - t1 < 5000, `${Date.now() - t1} ms to stop`)
const lastReq = fx.log.at(-1)?.t ?? 0
await sleep(2500)
check('and no request follows the cancel', (fx.log.at(-1)?.t ?? 0) === lastReq || (fx.log.at(-1)?.t ?? 0) - t1 < 2000)

// ---------------------------------------------------------------- the dialog, the plan approval, the downloads
ui = await openUi(h)
const { page } = ui
await page.waitForTimeout(500)
await page.click('button:has-text("Site grabber")')
await page.waitForSelector('text=Site grabber · Start page', { timeout: 10000 })
await page.fill('label:has-text("Project name") input', 'fixture handbook')
await page.fill('label:has-text("Start page") input', base)
await page.click('button:has-text("Next")') // how deep
await page.click('button:has-text("Next")') // file types
await page.fill('label:has-text("File types to fetch") input', 'pdf zip')
await page.click('button:has-text("Next")') // rules
await page.click('button:has-text("Next")') // preview
await page.waitForSelector('.probe', { timeout: 120000 })
const probeText = await page.textContent('.probe')
check('the dialog shows what it would read and fetch', /Would read\s*\d+ pages/.test(probeText ?? '') && /fetch\s*4 files/.test(probeText ?? ''), probeText?.replace(/\s+/g, ' '))
const dialogText = await page.textContent('.dialog, [role=dialog]')
check('it shows robots.txt facts, skipped reasons and who holds the approval', /robots\.txt read/.test(dialogText ?? '') && /Skipped:/.test(dialogText ?? '') && /held by this app/.test(dialogText ?? ''), (dialogText ?? '').replace(/\s+/g, ' ').slice(0, 200))
await page.screenshot({ path: `${out}/01-preview.png` })
await page.click('button:has-text("Request the run")')
await page.waitForSelector('text=Decide this request', { timeout: 20000 })
const facts = await page.textContent('.facts')
check('the plan approval shows its facts, held by this app', /workflow|plan/i.test(facts ?? '') && /fixture handbook|127\.0\.0\.1/.test(facts ?? ''), (facts ?? '').replace(/\s+/g, ' ').slice(0, 200))
await page.screenshot({ path: `${out}/02-approval.png` })
const bind = (await page.textContent('.mono.bind'))?.trim() ?? ''
if (await page.locator('label.typed input').count()) await page.fill('label.typed input', bind)
await page.waitForSelector('text=Armed.', { timeout: 10000 })
await page.click('.decide-grid .btn.decide:not(:has-text("Deny")):not(:has-text("Never")) >> nth=0')
await page.waitForSelector('text=Decided.', { timeout: 20000 })
await page.waitForTimeout(2500)
console.log('  held approvals:', JSON.stringify((await h.get('/svc/v1/approvals')).items.map((a) => [a.state, a.facts.kind, a.decided?.via])), '| toasts:', (await page.locator('.toast, [role=status]').allTextContents()).join(' / ').replace(/\s+/g, ' ').slice(0, 300), '| page errors:', ui.errors.slice(0, 3).join(' | '), '| jobs:', (await h.get('/api/v1/jobs')).items.length)
await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/downloads`)
await page.waitForTimeout(1500)
console.log('  rows:', (await page.locator('tbody tr').allTextContents()).map((t) => t.replace(/\s+/g, ' ').slice(0, 140)).join(' || '))
const ids = await until(async () => {
  const items = (await h.get('/api/v1/jobs')).items
  return items.length >= 4 && items.every((j) => ['completed', 'failed'].includes(j.state)) ? items : false
}, { timeoutMs: 120000, what: 'the grabbed files' })
check('after the approval the four files download', ids.length === 4 && ids.every((j) => j.state === 'completed'), ids.map((j) => `${j.filename}:${j.state}`).join(','))
const good = ids.every((j) => {
  const { size, seed } = siteFile(j.filename)
  return j.bytes_total === size && j.sha256 === sha256(seed, size) && fileSha(j.path) === j.sha256
})
check('each has the exact bytes the site serves', good)
await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/downloads`)
await page.waitForTimeout(1500)
await page.screenshot({ path: `${out}/03-downloads.png` })
// The project persists.
await sleep(1600)
const state = await h.get('/svc/v1/state')
const projects = state.slices.projects?.value ?? []
check('the grabber project is kept', projects.some((q) => q.name === 'fixture handbook' && q.state === 'running'), JSON.stringify(projects.map((q) => [q.name, q.state])))
check('no console errors', ui.errors.length === 0, ui.errors.slice(0, 2).join(' | '))
await ui.close()
await h.stop()
await fx.close()
console.log(failed ? `SCENARIO 6: ${failed} FAILED` : 'SCENARIO 6: PASSED')
process.exit(failed ? 1 : 0)

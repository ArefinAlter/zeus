// Real UI + native daemon: typing during queue refresh and late probe responses.
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { download, startHarness, until, sleep } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const out = process.env.ZEUS_INTAKE_EVIDENCE
assert.ok(out && !existsSync(out), 'a new evidence directory is required')
mkdirSync(out, { recursive: true })
const pdf = Buffer.from('%PDF-1.7\n%%EOF\n'), seen = [], held = new Map(), results = []
const server = createServer((req, res) => {
  const path = new URL(req.url, 'http://fixture').pathname
  seen.push(path)
  const send = () => {
    if (res.destroyed) return
    res.writeHead(200, { 'content-type': 'application/pdf', 'content-length': pdf.length,
      'content-disposition': `attachment; filename="${path === '/new' ? 'New Report.pdf' : path.startsWith('/slow') ? 'Old Report.pdf' : 'download.pdf'}"` })
    res.end(pdf)
  }
  if (path.startsWith('/slow')) held.set(path, send)
  else send()
})
await new Promise(r => server.listen(0, '127.0.0.1', r))
const origin = `http://127.0.0.1:${server.address().port}`
const h = await startHarness({ home: join(out, 'profile') })
let ui, failure
try {
  await h.ready(); ui = await openUi(h)
  const page = ui.page
  const address = () => page.getByPlaceholder('https://files.example.org/archive.zip', { exact: true })
  const saveAs = () => page.getByLabel('Save as', { exact: true })
  const close = async () => { if (await page.getByRole('dialog', { name: 'Add URL', exact: true }).count()) await page.getByRole('button', { name: 'Close', exact: true }).click() }
  const run = async (name, action) => {
    await page.getByRole('button', { name: 'Add URL', exact: true }).first().click()
    let failure
    try { await action() } catch (error) { failure = String(error) }
    await page.screenshot({ path: join(out, name + '.png') })
    results.push({ name, passed: !failure, ...(failure ? { failure } : {}) })
    for (const send of held.values()) send()
    held.clear(); await close()
  }
  await run('typing-during-queue-refresh', async () => {
    const url = `${origin}/download?id=typing-through-background-refresh-1234567890`
    await address().focus()
    const typing = page.keyboard.type(url, { delay: 100 })
    await sleep(750)
    await download(h, [{ url: origin + '/background' }])
    await typing
    const state = { expected: url, actual: await address().inputValue(), focused: await address().evaluate(el => document.activeElement === el) }
    writeFileSync(join(out, 'typing-state.json'), JSON.stringify(state, null, 2))
    assert.equal(state.actual, url, 'queue refresh must not truncate keyboard input')
    assert.ok(state.focused, 'queue refresh must not steal address focus')
    await until(async () => (await saveAs().inputValue()) === 'download.pdf', { what: 'final typed URL metadata' })
    await page.keyboard.type(' ')
    await until(async () => (await saveAs().inputValue()) === 'download.pdf' && !(await page.getByRole('button', { name: 'Checking…', exact: true }).count()), { what: 'normalized URL edit still receives metadata' })
    await page.keyboard.press('Escape')
    assert.equal(await page.getByRole('dialog', { name: 'Add URL', exact: true }).count(), 0, 'Escape closes the dialog after refresh')
    assert.ok(await page.getByRole('button', { name: 'Add URL', exact: true }).first().evaluate(el => document.activeElement === el), 'closing restores focus to the opener')
  })
  await run('late-probe-preserves-user-fields', async () => {
    await address().fill(origin + '/slow-edit')
    await until(() => held.has('/slow-edit'), { what: 'delayed real probe arrives' })
    const folder = join(h.home, 'chosen'); mkdirSync(folder)
    await saveAs().fill('My chosen filename.pdf')
    await page.getByLabel('Folder', { exact: true }).fill(folder)
    held.get('/slow-edit')(); held.delete('/slow-edit')
    await until(async () => !(await page.getByRole('button', { name: 'Checking…', exact: true }).count()), { what: 'delayed probe completes' })
    await sleep(300)
    assert.equal(await saveAs().inputValue(), 'My chosen filename.pdf', 'late metadata must preserve the user filename')
    assert.equal(await page.getByLabel('Folder', { exact: true }).inputValue(), folder, 'late metadata must preserve the user folder')
  })
  await run('new-url-supersedes-old-probe', async () => {
    await address().fill(origin + '/slow-old')
    await until(() => held.has('/slow-old'), { what: 'old probe arrives' })
    await address().fill(origin + '/new')
    await until(async () => (await saveAs().inputValue()) === 'New Report.pdf', { timeoutMs: 8000, what: 'new URL completes while the old probe is still held' })
    held.get('/slow-old')(); held.delete('/slow-old')
    await sleep(750)
    assert.equal(await saveAs().inputValue(), 'New Report.pdf', 'a stale response arriving last must not replace new metadata')
    assert.ok(seen.includes('/new'), 'a new URL must be probed after replacing an in-flight request')
  })
  await run('explicit-name-equal-to-url-basename', async () => {
    await address().fill(origin + '/explicit')
    await until(async () => (await saveAs().inputValue()) === 'download.pdf', { what: 'server filename suggestion' })
    await saveAs().fill('explicit')
    const confirm = page.getByRole('button', { name: 'Save here', exact: true })
    if (await confirm.isVisible()) await confirm.click()
    await page.getByRole('button', { name: 'Start download', exact: true }).click()
    const job = await until(async () => (await h.get('/api/v1/jobs')).items.find(j => j.state === 'completed' && j.url_redacted === origin + '/explicit'), { what: 'explicitly named download completes' })
    assert.equal(job.filename, 'explicit', 'an explicit name matching the URL basename remains an explicit choice')
    assert.deepEqual(readFileSync(job.path), pdf, 'the explicitly named file retains exact response bytes')
    await until(async () => page.getByRole('dialog', { name: 'Download Status: explicit', exact: true }).count(), { what: 'progress popup follows the actual daemon job' })
    await until(async () => page.getByRole('button', { name: 'Open File', exact: true }).isVisible(), { timeoutMs: 10000, what: 'the UI observes completion and exposes the actual completed file' })
    const layouts = []
    for (const [width, height] of [[1360, 900], [960, 600], [390, 740]]) {
      await page.setViewportSize({ width, height })
      await sleep(150)
      const layout = await page.locator('.idm-popup').evaluate(popup => {
        const rect = selector => { const r = popup.querySelector(selector).getBoundingClientRect(); return { left: r.left, right: r.right, top: r.top, bottom: r.bottom, width: r.width, height: r.height } }
        const dialog = popup.closest('[role="dialog"]')
        return { icon: rect('.idm-header > svg'), title: rect('.idm-title'), percentage: rect('.idm-header-pct'),
          rateIcon: rect('.idm-rate > svg'), footer: rect('.idm-footer'),
          innerWidth: dialog.clientWidth, scrollWidth: dialog.scrollWidth, windowWidth: innerWidth }
      })
      layouts.push({ width, height, ...layout })
      assert.ok(layout.icon.width <= 64 && layout.icon.height <= 64, 'file icon must not displace download details')
      assert.ok(layout.rateIcon.width <= 24 && layout.rateIcon.height <= 24, 'rate icon must not displace controls')
      assert.ok(layout.title.width > 0 && layout.title.right <= layout.percentage.left, 'filename and percentage must not overlap')
      assert.ok(layout.scrollWidth <= layout.innerWidth + 1, 'progress details must not overflow horizontally')
      if (height === 900) assert.ok(layout.footer.bottom < height, 'desktop actions must be visible without scrolling past oversized content')
      await page.getByRole('button', { name: 'Hide', exact: true }).scrollIntoViewIfNeeded()
      await page.screenshot({ path: join(out, `progress-layout-${width}.png`) })
    }
    writeFileSync(join(out, 'progress-layout.json'), JSON.stringify(layouts, null, 2) + '\n')
    await page.setViewportSize({ width: 1360, height: 900 })
  })
  assert.deepEqual(ui.errors, [], 'no UI errors')
} catch (error) {
  failure = error
} finally {
  for (const send of held.values()) send()
  try { await ui?.close() } catch (error) { failure ??= error }
  try { await h.stop() } catch (error) { failure ??= error }
  server.closeAllConnections(); await new Promise(r => server.close(r))
  writeFileSync(join(out, 'result.json'), JSON.stringify({ passed: !failure && results.length === 4 && results.every(r => r.passed), results, ...(failure ? { failure: String(failure) } : {}) }, null, 2) + '\n')
}
if (failure) throw failure
console.log(JSON.stringify(results, null, 2))
assert.ok(results.length === 4 && results.every(r => r.passed), 'intake regressions must all pass')
console.log('SCENARIO 33: PASSED')

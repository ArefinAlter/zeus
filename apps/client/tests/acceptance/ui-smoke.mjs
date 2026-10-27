// The interface in Chromium against the real engine: Add URL end to end, then every top-level page and every Settings
// page opens without a console error. Screenshots go to the folder named by --out.
// Run: node tests/acceptance/ui-smoke.mjs [--out DIR]

import { mkdirSync } from 'node:fs'

import { startFixtures } from '../fixture-server.mjs'
import { startHarness } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const out = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : 'D:/verne-build/zeus/scratch/ui-smoke'
mkdirSync(out, { recursive: true })
const fx = await startFixtures(0)
const h = await startHarness({ home: 'D:/verne-build/zeus/scratch/ui1', fresh: true })
await h.ready()
const ui = await openUi(h)
const { page } = ui
let failed = 0
const check = (name, ok, detail = '') => {
  if (!ok) failed++
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}

await page.waitForFunction(() => !document.querySelector('.conn-banner'), null, { timeout: 30000 }).catch(() => undefined)
await page.screenshot({ path: `${out}/01-downloads-empty.png` })

// Add URL, check, start
await page.click('text=Add URL >> nth=0')
await page.fill('input.mono', `${fx.url}/cd/x?size=${24 * 1024 * 1024}&seed=3&as=Annual%20Report.pdf&rate=3072`)
await page.click('button:has-text("Check")')
await page.waitForSelector('.probe', { timeout: 20000 })
const probeText = await page.textContent('.probe')
check('the check shows the server\'s answer', /206/.test(probeText ?? ''), probeText?.trim())
const saveAs = await page.inputValue('label:has-text("Save as") input')
check('the name from the server is offered', saveAs === 'Annual Report.pdf', saveAs)
await page.screenshot({ path: `${out}/02-add-url-checked.png` })
await page.click('button:has-text("Start download")')
await page.waitForSelector('text=Annual Report.pdf', { timeout: 20000 })
await page.waitForSelector('.state-completed, :text("completed")', { timeout: 90000 }).catch(() => undefined)
await page.screenshot({ path: `${out}/03-download.png` })
const rows = await page.locator('tbody tr, .dl-row, [role=row]').count()
check('the download appears in the list', rows >= 1, `${rows} rows`)

const pages = ['downloads', 'grabber', 'queues', 'files', 'agent', 'skills', 'approvals', 'connectors']
for (const p of pages) {
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/${p}`)
  await page.waitForTimeout(900)
  await page.screenshot({ path: `${out}/page-${p}.png` })
}
for (const s of ['general', 'appearance', 'keybindings', 'filetypes', 'saveto', 'downloads', 'connection', 'sitelogins', 'browser', 'scheduler', 'limiter', 'notifications', 'index', 'providers', 'agent', 'storage', 'security', 'daemon', 'about']) {
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/settings/${s}`)
  await page.waitForTimeout(500)
  await page.screenshot({ path: `${out}/settings-${s}.png` })
}
check('no console errors on any page', ui.errors.length === 0, ui.errors.slice(0, 3).join(' | '))
await ui.close()
await h.stop()
await fx.close()
console.log(failed ? `UI SMOKE: ${failed} FAILED` : 'UI SMOKE: PASSED')
process.exit(failed ? 1 : 0)

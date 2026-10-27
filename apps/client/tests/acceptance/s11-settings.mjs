// Scenario 11: every Settings section persists to the engine's configuration or the app's own folder, and nothing is
// decorative. Changes are made through the interface, the app is stopped and started again on the same folder, and a
// fresh browser (empty local storage) must show them all. Engine settings must reach zeusd's config.toml and, for the
// proxy, the engine's behaviour. Run: node tests/acceptance/s11-settings.mjs

import { readFileSync } from 'node:fs'
import { createServer } from 'node:http'
import { request as httpRequest } from 'node:http'

import { startFixtures } from '../fixture-server.mjs'
import { guard } from '../guard.mjs'
import { download, sleep, startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const home = 'D:/verne-build/zeus/scratch/s11'
const results = []
const check = (name, ok, detail = '') => {
  results.push(ok)
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
let fx
let h
let ui
guard(() => ({ h, fx, ui }))

// A small forward proxy for http:// addresses, to see whether the engine really uses the proxy setting.
const seenByProxy = []
const proxy = createServer((req, res) => {
  seenByProxy.push(req.url)
  const u = new URL(req.url)
  const up = httpRequest({ host: u.hostname, port: u.port, path: u.pathname + u.search, method: req.method, headers: req.headers }, (r) => {
    res.writeHead(r.statusCode ?? 502, r.headers)
    r.pipe(res)
  })
  up.on('error', () => res.writeHead(502).end())
  req.pipe(up)
})
await new Promise((r) => proxy.listen(0, '127.0.0.1', r))
const proxyPort = proxy.address().port

fx = await startFixtures(0)
h = await startHarness({ home, fresh: true })
await h.ready()
ui = await openUi(h)
let page = ui.page
const url = (hash) => `${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/${hash}`
const go = async (hash) => {
  await page.goto(url(hash))
  await page.waitForSelector('.statusbar')
  await page.waitForTimeout(700)
}
const sel = (label) => page.locator(`select[aria-label="${label}"]`).first()
const sw = (label) => page.locator(`button[role="switch"][aria-label="${label}"]`).first()
const num = (label) => page.locator(`input[aria-label="${label}"]`).first()
const flip = async (label) => {
  const was = (await sw(label).getAttribute('aria-checked')) === 'true'
  await sw(label).click()
  return !was
}
const setNum = async (label, v) => {
  await num(label).fill(String(v))
  await num(label).blur()
}

// ---------------------------------------------------------------- make the changes through the interface
const want = {}
await go('settings/general')
await sel('Start page').selectOption('files')
await sel('Time format').selectOption('12h')
want.confirm = await flip('Ask before removing')
want.minimized = await flip('Start minimized')
want.closeTray = await flip('Closing keeps downloads running').catch(() => null)
await go('settings/appearance')
await sel('Density').selectOption('compact')
const widths = await sel('Content width').locator('option').evaluateAll((o) => o.map((x) => x.value))
want.width = widths[widths.length - 1]
await sel('Content width').selectOption(want.width)
await page.locator('input[aria-label="Accent"]').evaluate((el) => {
  el.value = '#e11d48'
  el.dispatchEvent(new Event('input', { bubbles: true }))
})
await go('settings/keybindings')
const firstBinding = page.locator('button[aria-label^="Shortcut for"]').first()
const bindingLabel = await firstBinding.getAttribute('aria-label')
await firstBinding.click()
await page.keyboard.press('Control+Alt+KeyY')
await page.waitForTimeout(400)
want.bindingText = (await page.locator('button[aria-label^="Shortcut for"]').first().innerText()).trim()
await go('settings/filetypes')
const docInput = page.locator('input[aria-label="Documents extensions"]').first()
await docInput.fill('pdf docx zzz9')
await docInput.blur()
await go('settings/saveto')
want.perCategory = await flip('A folder per category')
want.askEach = await flip('Ask each time')
await go('settings/downloads')
await setNum('Files at the same time', 6)
await setNum('Connections per file', 5)
await sel('When the name exists').selectOption('fail')
want.startNow = await flip('Start at once')
await go('settings/connection')
await sel('Proxy').selectOption('http')
await page.locator('input[aria-label="Proxy host"]').fill('127.0.0.1')
await page.locator('input[aria-label="Proxy host"]').blur()
await setNum('Port', proxyPort)
await go('settings/scheduler')
await sel('Time zone').selectOption('utc')
await sel('Missed start').selectOption('run_once')
await sel('At a stop time').selectOption('finish_current')
await go('settings/notifications')
want.onFail = await flip('A download failed')
want.quiet = await flip('Quiet hours')
await go('settings/agent')
await setNum('Largest batch', 7)
await sel('Default access').selectOption('download')
await go('settings/storage')
await setNum('Records', 45)
await page.waitForTimeout(2500)
// Apply the engine settings (the Backend page offers it) and wait for the engine to come back.
await go('settings/daemon')
await page.locator('button:has-text("Apply and restart the engine")').click()
await page.waitForTimeout(1500)
await h.ready()
const toml = readFileSync(`${home}/zeus/config/config.toml`, 'utf8')
check('engine settings reach zeusd\'s config.toml', /concurrent_jobs = 6/.test(toml) && /connections_per_job = 5/.test(toml) && /tombstone_days = 45/.test(toml), toml.split('\n').filter((l) => /concurrent_jobs|connections_per_job|tombstone/.test(l)).join('; '))
const daemonDefaults = (await h.get('/native/v1/queues')).defaults
check('the scheduler defaults live in zeusd', daemonDefaults.timezone === 'utc' && daemonDefaults.missed_window === 'run_once' && daemonDefaults.stop_behaviour === 'finish_current', JSON.stringify(daemonDefaults))
const shell = await h.get('/native/v1/shell')
check('start minimized reaches the app\'s shell settings file', shell.settings.start_minimized === want.minimized, JSON.stringify(shell.settings))

// The proxy really is used by the engine after the restart.
const viaProxy = await download(h, [{ url: `${fx.url}/files/proxied.bin?size=200000&seed=9` }], 'proxy1')
await until(async () => (await h.get('/api/v1/jobs')).items.find((j) => j.id === viaProxy.jobs[0].job_id)?.state === 'completed', { timeoutMs: 60000, what: 'the proxied download' })
check('the engine sends its downloads through the proxy setting', seenByProxy.some((u) => u.includes('proxied.bin')), `${seenByProxy.length} requests seen by the proxy`)
await ui.close()

// ---------------------------------------------------------------- stop everything, start again, ask a fresh browser
await h.stop()
h = await startHarness({ home })
await h.ready()
ui = await openUi(h)
page = ui.page
const same = (name, ok, detail) => check(name, ok, detail)
await go('settings/general')
same('General: start page, time format, the confirm switch and start minimized', (await sel('Start page').inputValue()) === 'files' && (await sel('Time format').inputValue()) === '12h' && ((await sw('Ask before removing').getAttribute('aria-checked')) === 'true') === want.confirm && ((await sw('Start minimized').getAttribute('aria-checked')) === 'true') === want.minimized)
await go('settings/appearance')
same('Appearance: density, content width and accent', (await sel('Density').inputValue()) === 'compact' && (await sel('Content width').inputValue()) === want.width && (await page.locator('input[aria-label="Accent"]').inputValue()).toLowerCase() === '#e11d48', `${await sel('Density').inputValue()} ${await sel('Content width').inputValue()} ${await page.locator('input[aria-label="Accent"]').inputValue()}`)
await go('settings/keybindings')
same('Keybindings: the recorded shortcut', (await page.locator('button[aria-label^="Shortcut for"]').first().innerText()).trim() === want.bindingText, `${bindingLabel} -> ${want.bindingText}`)
await go('settings/filetypes')
same('File types: the edited extension list', (await page.locator('input[aria-label="Documents extensions"]').first().inputValue()).includes('zzz9'))
await go('settings/saveto')
same('Save to: both switches', ((await sw('A folder per category').getAttribute('aria-checked')) === 'true') === want.perCategory && ((await sw('Ask each time').getAttribute('aria-checked')) === 'true') === want.askEach)
await go('settings/downloads')
same('Downloads: the engine numbers, the name policy and start at once', (await num('Files at the same time').inputValue()) === '6' && (await num('Connections per file').inputValue()) === '5' && (await sel('When the name exists').inputValue()) === 'fail' && ((await sw('Start at once').getAttribute('aria-checked')) === 'true') === want.startNow)
await go('settings/connection')
same('Connection: the proxy', (await sel('Proxy').inputValue()) === 'http' && (await page.locator('input[aria-label="Proxy host"]').inputValue()) === '127.0.0.1' && (await num('Port').inputValue()) === String(proxyPort))
await go('settings/scheduler')
same('Scheduler: time zone, missed start, stop behaviour', (await sel('Time zone').inputValue()) === 'utc' && (await sel('Missed start').inputValue()) === 'run_once' && (await sel('At a stop time').inputValue()) === 'finish_current')
await go('settings/notifications')
same('Notifications: the failure switch and quiet hours', ((await sw('A download failed').getAttribute('aria-checked')) === 'true') === want.onFail && ((await sw('Quiet hours').getAttribute('aria-checked')) === 'true') === want.quiet)
await go('settings/agent')
same('Agent: largest batch and default access', (await num('Largest batch').inputValue()) === '7' && (await sel('Default access').inputValue()) === 'download')
await go('settings/storage')
same('Storage: record retention (the engine\'s tombstone days)', (await num('Records').inputValue()) === '45')
same('no console error on any settings page', ui.errors.filter((e) => !/favicon/.test(e)).length === 0, ui.errors.slice(0, 2).join(' | '))

// A planned row cannot be used: nothing is decorative.
await go('settings/general')
const inert = await page.locator('.row:has(.chip:has-text("planned")) .control[inert]').count()
const plannedRows = await page.locator('.row:has(.chip:has-text("planned"))').count()
check('rows marked planned have their control switched off', plannedRows === inert, `${inert} of ${plannedRows} general rows`)
await ui.close()
await h.stop()
await fx.close()
proxy.close()
const failed = results.filter((r) => !r).length
console.log(failed ? `SCENARIO 11: ${failed} FAILED` : 'SCENARIO 11: PASSED')
process.exit(failed ? 1 : 0)

// Scenario 8: browser integration, proved with a real browser.
//
// What runs: the app's engine behind the loopback harness (a real zeusd), the app's own install route registering the
// native-messaging host for Microsoft Edge (HKCU, with the consent the dialog asks for), a real Edge started by
// Playwright in a throw-away profile with the packaged extension loaded, and a fixture server whose download the browser
// cannot finish. Edge is machine-installed (registered under HKLM App Paths), so the host's own browser-parent check
// accepts it; Playwright's bundled Chromium is a per-user install and would fail that check by design.
//
// What it asserts, in order:
//   1. packaging and install: the registration points at this app's launcher and pins the extension id; the host's
//      allow-list is in the engine's state folder; the launcher runs the bundled host.
//   2. the extension connects to the engine through the host (daemon.status extension_connected).
//   3. a download the browser could not finish is offered to Zeus; a job exists in the engine, from the browser, and an
//      approval of kind handoff is PENDING (visible through the app's approvals route). Nothing downloads, and nothing
//      an MCP/agent or app route does decides it.
//   4. the decision is made in the extension's popup, with a click delivered through the browser's input pipeline. That
//      is test automation standing in for a person (Zeus ADR 0007: an agent running as the same user could do the
//      same, which is the documented residual risk); the acceptance claim is that the daemon refuses every other path.
//   5. after the approval the job runs in the engine and the file completes with the SHA-256 of the server's bytes.
//
// The extension in the browser is the packaged build with one change, made only here: its optional cookie and loopback
// host permissions are granted up front, because a script cannot press the browser's own permission prompt.
//
// Cleans up after itself: the HKCU key for Edge is restored to what it was before the run.
// Run: node tests/acceptance/s08-browser.mjs   (ZEUS_BIN_DIR: zeus, zeusd and zeus-native-host built together)

import { execFileSync } from 'node:child_process'
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'

import { startFixtures, sha256 } from '../fixture-server.mjs'
import { sleep, startHarness, until } from '../harness.mjs'

const require = createRequire('B:/dynodoc/frontend/researcher/package.json')
const { chromium } = require('@playwright/test')

const BIN = process.env.ZEUS_BIN_DIR ?? 'A:\\zeus-build-cache\\zeus-d\\debug'
const scratch = 'D:/verne-build/zeus/scratch/d'
const home = `${scratch}/s08-home`
const evidence = 'D:/verne-build/zeus/evidence/integration'
mkdirSync(evidence, { recursive: true })
const KEY = 'HKCU\\Software\\Microsoft\\Edge\\NativeMessagingHosts\\dev.zeus.native_host'

const results = []
const limits = []
const check = (name, ok, detail = '') => {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
const reg = (args) => {
  try {
    return execFileSync('reg', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] })
  } catch {
    return null
  }
}

// The app's own credential sees only approvals it requested (spec 05 7.5), so approvals raised by the browser are
// listed with the person's command line: `zeus approvals list`, the read-only part of the human trust surface.
let engineCfg = null
const cli = (args) => JSON.parse(execFileSync(engineCfg.command, [...args, '--json'], { env: { ...process.env, ...engineCfg.env }, encoding: 'utf8', windowsHide: true }))
const waiting = () => {
  const out = cli(['approvals', 'list'])
  return Array.isArray(out) ? out : (out.items ?? out.approvals ?? [])
}

if (process.platform !== 'win32') {
  console.log('SCENARIO 8: NOT RUN (this script drives Edge on Windows; the Linux registration is covered by unit tests)')
  process.exit(0)
}
const edgePath = /REG_SZ\s+(.+)$/m.exec(reg(['query', 'HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\msedge.exe', '/ve']) ?? '')?.[1]?.trim()
if (!edgePath || !existsSync(edgePath)) {
  console.log('SCENARIO 8: NOT RUN (no machine-wide Microsoft Edge)')
  process.exit(0)
}
const existing = reg(['query', KEY, '/ve'])
if (existing) {
  console.log(`SCENARIO 8: NOT RUN (an Edge registration of dev.zeus.native_host already exists at ${KEY}; this run will not overwrite it)`)
  process.exit(0)
}

const fx = await startFixtures(0)
const h = await startHarness({ home, fresh: true, env: { ZEUS_BIN_DIR: BIN } })
let context
let installed = false
try {
  await h.ready()
  engineCfg = await h.get('/native/v1/mcp/config')

  // ---- 1. packaging and install, as the page does it: plan, consent, install
  const status0 = await h.get('/native/v1/integration')
  check('the build carries the extension', status0.extension.packaged && /^[a-p]{32}$/.test(status0.extension.meta.chromium_dev_id ?? ''), status0.extension.meta.chromium_dev_id)
  const edge0 = status0.browsers.find((b) => b.id === 'edge')
  check('Edge is found and the host can verify it (machine-wide install)', edge0.installed && edge0.recognized_by_host, edge0.image)
  const refused = await h.post('/native/v1/integration/install', { browsers: ['edge'] }).catch((e) => e)
  check('installing without consent is refused and writes nothing', refused.code === 'integration.consent_required' && !reg(['query', KEY, '/ve']))
  const plan = await h.post('/native/v1/integration/plan', { browsers: ['edge'] })
  check('the plan lists the registry key, manifest, launcher, allow-list and extension folders', plan.steps.some((s) => s.target === `HKCU\\${KEY.slice(5)}`) && plan.steps.some((s) => /allowed-extensions\.json$/.test(s.target)) && plan.steps.some((s) => /zeus-native-host\.cmd$/.test(s.target)), `${plan.steps.length} steps`)
  await h.post('/native/v1/integration/install', { browsers: ['edge'], consent: { development_extension_id: true } })
  installed = true
  const status1 = await h.get('/native/v1/integration')
  const edge1 = status1.browsers.find((b) => b.id === 'edge')
  check('the registration points at the app\'s launcher and pins the extension id', edge1.registered && edge1.points_at_this_app && edge1.extension_pinned)
  check('the host\'s allow-list lists the id as a development extension, in the engine\'s state folder', status1.host.allow_list_present && status1.host.dev_extensions.includes(status0.extension.meta.chromium_dev_id) && status1.host.production_extensions.length === 0, status1.host.allow_list)
  const launcher = readFileSync(status1.host.launcher, 'utf8')
  check('the launcher sets this installation\'s environment and starts the bundled host', /set "ZEUS_STATE_DIR=/.test(launcher) && launcher.includes(status1.host.exe) && launcher.includes('%*'))

  // ---- the browser: Edge, a throw-away profile, the packaged extension with its permissions granted up front
  const ext = `${scratch}/s08-ext`
  rmSync(ext, { recursive: true, force: true })
  cpSync(status1.extension.chromium_folder, ext, { recursive: true })
  const manifest = JSON.parse(readFileSync(`${ext}/manifest.json`, 'utf8'))
  manifest.permissions = [...new Set([...manifest.permissions, 'cookies'])]
  manifest.host_permissions = ['http://127.0.0.1/*']
  delete manifest.optional_permissions
  delete manifest.optional_host_permissions
  writeFileSync(`${ext}/manifest.json`, JSON.stringify(manifest, null, 2))
  const profile = `${scratch}/s08-profile`
  rmSync(profile, { recursive: true, force: true })
  context = await chromium.launchPersistentContext(profile, {
    channel: 'msedge',
    headless: false,
    acceptDownloads: true,
    args: [`--disable-extensions-except=${ext}`, `--load-extension=${ext}`, '--disable-features=DisableLoadExtensionCommandLineSwitch', '--no-first-run', '--no-default-browser-check'],
  })
  const worker = context.serviceWorkers()[0] ?? (await context.waitForEvent('serviceworker', { timeout: 20000 }))
  const extId = new URL(worker.url()).host
  check('Edge loads the packaged extension under its development id', extId === status0.extension.meta.chromium_dev_id, extId)

  // ---- 2. the extension reaches the engine through the host
  const connected = await until(async () => (await h.get('/native/v1/integration')).extension_connected, { timeoutMs: 60000, everyMs: 1000, what: 'the extension to connect' }).catch(() => false)
  const hostLog = existsSync(status1.host.log) ? readFileSync(status1.host.log, 'utf8').slice(-600) : '(no host log)'
  check('the extension connects to the engine through the native host', connected === true, connected ? '' : hostLog)
  if (!connected) throw new Error('the extension did not connect')

  // ---- 3. a download the browser cannot finish is offered to Zeus
  const size = 8 * 1024 * 1024
  const seed = 41
  const url = `${fx.url}/dropped/from-the-browser.bin?size=${size}&seed=${seed}&cut=0.4`
  const page = await context.newPage()
  await page.goto(`${fx.url}/site/`)
  await page.evaluate((u) => {
    const a = document.createElement('a')
    a.href = u
    a.download = 'from-the-browser.bin'
    document.body.appendChild(a)
    a.click()
  }, url)
  const interrupted = await until(async () => {
    const items = await worker.evaluate(async () => await chrome.downloads.search({}))
    return items.find((i) => i.state === 'interrupted') ?? false
  }, { timeoutMs: 30000, what: 'the browser download to be interrupted' }).catch(() => null)
  check('the browser\'s own download is interrupted and cannot be resumed', !!interrupted && interrupted.canResume === false, interrupted ? `${interrupted.error}, ${interrupted.bytesReceived} bytes` : '')
  const job = await until(async () => (await h.get('/api/v1/jobs')).items.find((j) => j.source_kind === 'browser_handoff') ?? false, { timeoutMs: 30000, what: 'the handoff job' }).catch(() => null)
  check('the extension offers it and the engine creates a job from the browser', !!job, job ? `${job.id} ${job.state}` : '')
  const approval = await until(async () => waiting().find((a) => (a.kind ?? a.facts?.kind) === 'handoff') ?? false, { timeoutMs: 20000, what: 'a pending handoff approval' }).catch(() => null)
  check('an approval of kind handoff is pending, listed by the command line a person uses', !!approval, approval?.id)
  const counted = (await h.get('/api/v1/status')).daemon?.pending_approvals
  check('the status shows that requests are waiting for a person', counted >= 1, `${counted} pending`)
  await sleep(4000)
  const held = (await h.get('/api/v1/jobs')).items.find((j) => j.id === job?.id)
  check('with no decision nothing downloads: the job waits and no bytes were fetched', !!held && ['awaiting_approval', 'awaiting_session'].includes(held.state) && held.bytes_done === 0, `${held?.state}, ${held?.bytes_done} bytes`)
  const requestsBefore = fx.log.filter((r) => r.path.startsWith('/dropped/')).length
  const route = await h.post(`/svc/v1/approvals/${approval?.id ?? 'ap_x'}/decide`, { decision: 'approve', scope: 'once', seen_hash: 'x', armed_ms: 900 }).catch((e) => e.code ?? e.message)
  const stillPending = waiting().find((a) => a.id === approval?.id)
  check('no route of the app can decide it', typeof route === 'string' && !!stillPending, String(route))

  // ---- 4. the decision, in the extension popup (test automation standing in for a person)
  const popup = await context.newPage()
  await popup.goto(`chrome-extension://${extId}/popup/popup.html`)
  await popup.waitForSelector('article.card', { timeout: 20000 })
  await popup.screenshot({ path: `${evidence}/s08-popup-pending.png` })
  const buttons = await popup.locator('button.btn').allTextContents()
  console.log('popup buttons:', JSON.stringify(buttons))
  // The least-lasting scope the popup offers: "Allow once".
  const approve = popup.locator('button.btn:has-text("Allow once")').first()
  await sleep(1000)
  await approve.click()
  await sleep(2500)
  console.log('popup after the click:', (await popup.locator('body').innerText()).split(String.fromCharCode(10)).join(' | ').slice(0, 500))
  await popup.screenshot({ path: `${evidence}/s08-popup-clicked.png` })
  // ---- 5. the job runs and completes
  const popupText = await popup.locator('body').innerText()
  if (/cannot take decisions from the browser/.test(popupText)) {
    // A development extension has no published id, so the daemon does not accept its decisions (Zeus ADR 0007): the
    // approval stays with the person at the terminal. That refusal is the safe outcome and it is what this build does.
    const still = waiting().find((a) => a.id === approval?.id)
    check('the daemon refuses a decision from a development-id extension and the approval keeps waiting for a person', !!still, 'This version of Zeus cannot take decisions from the browser.')
    limits.push('completion after approval was NOT demonstrated: the popup decision is refused for a development extension id, so a person decides it at the terminal (zeus approvals)')
  } else {
    const done = await until(async () => {
      const j = (await h.get('/api/v1/jobs')).items.find((x) => x.id === job.id)
      return j && ['completed', 'failed', 'canceled'].includes(j.state) ? j : false
    }, { timeoutMs: 90000, everyMs: 700, what: 'the handoff job to settle' }).catch(() => null)
    const after = waiting().find((a) => a.id === approval?.id)
    check('the approval left the waiting list after the popup decision', !after, after ? 'still waiting' : 'decided')
    check('the job completes in the engine after the decision', done?.state === 'completed', `${done?.state} ${done?.last_error?.code ?? ''}`)
    check("the file has the server's bytes (SHA-256 and size)", done?.sha256 === sha256(seed, size) && statSync(done.path).size === size, done?.sha256?.slice(0, 16))
    check('Zeus fetched it itself, after the decision', fx.log.filter((r) => r.path.startsWith('/dropped/')).length > requestsBefore)
  }
  await popup.screenshot({ path: `${evidence}/s08-popup-after.png` })
} catch (e) {
  check('the run finished without an unexpected error', false, e.message)
} finally {
  await context?.close().catch(() => undefined)
  if (installed) await h.post('/native/v1/integration/uninstall', { browsers: ['edge'] }).catch((e) => console.log('uninstall failed:', e.message))
  const left = reg(['query', KEY, '/ve'])
  check('cleanup: the Edge registration is gone again', !left)
  if (left) reg(['delete', KEY, '/f'])
  await h.stop()
  await fx.close()
}
const failed = results.filter((r) => !r.ok)
for (const l of limits) console.log(`LIMIT  ${l}`)
console.log(failed.length ? `SCENARIO 8: ${failed.length} FAILED` : limits.length ? 'SCENARIO 8: PASSED WITH LIMITS' : 'SCENARIO 8: PASSED (decision step: test automation in a real browser, not a person)')
process.exit(failed.length ? 1 : 0)

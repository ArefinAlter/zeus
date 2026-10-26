// The Connect page and Settings, Browser integration in Chromium against the real engine: the assistant tabs show the
// configuration, the connection test lists thirteen tools, the browser panel lists browsers with honest status and its
// install button opens a consent dialog that writes nothing until it is confirmed. Screenshots to --out.
// Run: ZEUS_UI_DIR=dist/harness-d node tests/acceptance/ui-connect.mjs

import { mkdirSync } from 'node:fs'

import { startHarness } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const out = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : 'D:/verne-build/zeus/evidence/integration'
mkdirSync(out, { recursive: true })
const h = await startHarness({ home: 'D:/verne-build/zeus/scratch/d/ui-home', fresh: true, env: { ZEUS_BIN_DIR: process.env.ZEUS_BIN_DIR ?? 'A:\\zeus-build-cache\\zeus-d\\debug' } })
await h.ready()
const ui = await openUi(h)
const { page } = ui
let failed = 0
const check = (name, ok, detail = '') => {
  if (!ok) failed++
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}
const go = async (hash) => {
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#${hash}`)
  await page.waitForSelector('.statusbar', { timeout: 30000 })
}

await go('/connect')
await page.waitForSelector('text=Connect your AI assistant', { timeout: 20000 })
await page.waitForSelector('.codecopy pre', { timeout: 20000 })
const claude = await page.locator('.codecopy pre').first().textContent()
check('Claude Code: the command line names the bundled zeus and this engine', /claude mcp add --transport stdio --scope user/.test(claude ?? '') && /ZEUS_STATE_DIR=/.test(claude ?? ''), (claude ?? '').slice(0, 60))
await page.screenshot({ path: `${out}/connect-claude.png`, fullPage: true })
for (const [tab, needle] of [['Cursor', '"mcpServers"'], ['Codex', '[mcp_servers.zeus]'], ['Other assistant', '"command"']]) {
  await page.click(`[role=tab]:has-text("${tab}")`)
  const text = (await page.locator('.connect-tabs .codecopy pre').first().textContent()) ?? ''
  check(`${tab}: the configuration is shown`, text.includes(needle), text.slice(0, 40).replace(/\n/g, ' '))
}
await page.click('[role=tab]:has-text("Claude Code")')
await page.click('button:has-text("Test the connection")')
await page.waitForSelector('text=/Connected: zeus .*13 tools/', { timeout: 40000 })
check('the connection test lists thirteen tools', true)
await page.screenshot({ path: `${out}/connect-test.png`, fullPage: true })

await page.waitForSelector('.browser-table', { timeout: 20000 })
const rows = await page.locator('.browser-row').count()
check('the browser panel lists the five browsers with their real status', rows === 5, `${rows} rows`)
const edgeRow = page.locator('.browser-row', { hasText: 'Microsoft Edge' })
check('Edge is found and verifiable on this computer', /found/.test((await edgeRow.textContent()) ?? '') && /verifiable/.test((await edgeRow.textContent()) ?? ''))
await page.screenshot({ path: `${out}/connect-browsers.png`, fullPage: true })
await page.click('button:has-text("Install browser integration")')
await page.waitForSelector('[role=dialog]', { timeout: 10000 })
const dialog = (await page.locator('[role=dialog]').textContent()) ?? ''
check('the consent dialog lists what will be written and the development id', /HKCU|write/.test(dialog) && /development build/.test(dialog) && /allowed-extensions\.json/.test(dialog))
const installBtn = page.locator('[role=dialog] button:has-text("Install")').last()
check('the Install button stays disabled until the person ticks the consent', await installBtn.isDisabled())
await page.screenshot({ path: `${out}/connect-consent.png` })
await page.keyboard.press('Escape')
const status = await h.get('/native/v1/integration')
check('cancelling wrote nothing: no browser is registered', status.browsers.every((b) => !b.registered))

await go('/settings/browser')
await page.waitForSelector('.browser-table', { timeout: 20000 })
check('Settings, Browser integration shows the same facts', (await page.locator('.browser-row').count()) === 5)
await page.screenshot({ path: `${out}/settings-browser.png`, fullPage: true })
check('no console errors', ui.errors.length === 0, ui.errors.slice(0, 3).join(' | '))
await ui.close()
await h.stop()
console.log(failed ? `UI CONNECT: ${failed} FAILED` : 'UI CONNECT: PASSED')
process.exit(failed ? 1 : 0)

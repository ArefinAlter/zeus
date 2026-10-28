// Two-pane UI against actual scoped native file operations, in an explicit scratch profile.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, utimesSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'
import { openUi } from '../ui.mjs'

if (!process.env.ZEUS_FILE_MANAGER_EVIDENCE) throw new Error('Set ZEUS_FILE_MANAGER_EVIDENCE to a new scratch evidence directory')
const output = resolve(process.env.ZEUS_FILE_MANAGER_EVIDENCE)
const home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a new scratch profile for each acceptance run')
const read = join(home, 'read only'), managed = join(home, 'managed'), a = join(managed, 'A'), b = join(managed, 'B'), outside = join(home, 'outside')
for (const dir of [read, a, b, outside]) mkdirSync(dir, { recursive: true })
for (let n = 0; n < 1006; n++) writeFileSync(join(read, `fixture-${String(n).padStart(4, '0')}.txt`), `fixture${n}`)
for (const [folder, differing] of [[a, 'AAAA'], [b, 'BBBB']]) {
  writeFileSync(join(folder, 'same metadata.txt'), differing)
  writeFileSync(join(folder, 'twins.txt'), 'matching contents')
  utimesSync(join(folder, 'same metadata.txt'), 1700000000, 1700000000)
}
writeFileSync(join(b, 'move me.txt'), 'move contents')
writeFileSync(join(outside, 'outside canary.txt'), 'ungranted')
const checks = []
const check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui
try {
  h = await startHarness({ home }); await h.ready()
  const cfg = await h.get('/native/v1/index/config')
  cfg.roots.push(...[read, managed].map(path => ({ path, include: true, monitor_changes: true, rescan_every_hours: null, download: false, manage_files: path === managed })))
  await h.put('/native/v1/index/config', cfg)
  ui = await openUi(h)
  const page = ui.page
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
  await page.getByRole('button', { name: 'Two-pane manager', exact: true }).click()
  const left = page.getByRole('region', { name: 'Left file pane', exact: true }), right = page.getByRole('region', { name: 'Right file pane', exact: true })
  const choose = async (side, path) => {
    const pane = side === 'Left' ? left : right
    await pane.getByLabel(`${side} folder path`, { exact: true }).fill(path)
    await pane.getByRole('button', { name: 'Go', exact: true }).click()
    await until(async () => await pane.getByRole('grid').getAttribute('data-folder') === path && await pane.getByRole('grid').getAttribute('aria-busy') === 'false' && !(await pane.getByRole('status').innerText()).includes('Reading folder'), { what: `${side} listing` })
  }
  const select = async (pane, text) => { await pane.getByRole('gridcell', { name: text, exact: true }).click() }
  await choose('Left', read); await choose('Right', b)
  await left.getByRole('button', { name: 'Load more', exact: true }).waitFor()
  check('panes have independent bounded listings', (await left.getByRole('status').innerText()).includes('500 items') && (await right.getByRole('status').innerText()).includes('3 items'))
  await left.getByRole('button', { name: 'Load more', exact: true }).click()
  await until(async () => (await left.getByRole('status').innerText()).includes('1,000 items'))
  check('paging one pane leaves its peer unchanged', (await right.getByRole('status').innerText()).includes('3 items'))
  await left.getByRole('grid').focus(); await page.keyboard.press('End')
  await page.waitForTimeout(100)
  check('keyboard navigation virtualizes long folders with an active row', await left.getByRole('row').count() < 100 && !!await left.getByRole('grid').getAttribute('aria-activedescendant'))
  await page.keyboard.press('Home'); await page.waitForTimeout(100)
  await select(left, 'fixture-0000.txt')
  check('read-only source permits reviewed copy and refuses move', await left.getByRole('button', { name: 'Copy to right…', exact: true }).isEnabled() && await left.getByRole('button', { name: 'Move to right…', exact: true }).isDisabled())
  await left.getByRole('button', { name: 'Copy to right…', exact: true }).click()
  let dialog = page.getByRole('dialog')
  check('copy review presets the other pane without executing', await dialog.getByLabel('Folder', { exact: true }).inputValue() === b && !existsSync(join(b, 'fixture-0000.txt')) && (await dialog.innerText()).includes(join(read, 'fixture-0000.txt')))
  await dialog.getByRole('button', { name: 'Copy', exact: true }).click()
  await dialog.waitFor({ state: 'hidden' })
  check('native two-pane copy preserves exact source bytes', readFileSync(join(b, 'fixture-0000.txt'), 'utf8') === 'fixture0' && readFileSync(join(read, 'fixture-0000.txt'), 'utf8') === 'fixture0')
  await choose('Left', a); await choose('Right', b)
  await select(right, 'move me.txt')
  await right.getByRole('button', { name: 'Move to left…', exact: true }).click()
  dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Move', exact: true }).click(); await dialog.waitFor({ state: 'hidden' })
  check('managed move uses the actual destination and durable history', readFileSync(join(a, 'move me.txt'), 'utf8') === 'move contents' && !existsSync(join(b, 'move me.txt')) && (await h.get('/native/v1/files/history')).records.some(row => row.path === join(a, 'move me.txt') && row.can_undo))
  await page.getByRole('button', { name: 'Compare folders', exact: true }).click()
  await left.getByRole('gridcell', { name: 'Same metadata', exact: true }).first().waitFor()
  check('directory comparison does not claim equal content', (await page.locator('.fm-explanation').first().innerText()).includes('file contents are not compared'))
  await select(left, 'same metadata.txt'); await select(right, 'same metadata.txt')
  await page.getByRole('button', { name: 'Verify selected contents', exact: true }).click()
  await page.getByText('Selected file contents differ.', { exact: true }).waitFor()
  check('same size/date with different bytes fails content comparison', true)
  await select(left, 'twins.txt'); await select(right, 'twins.txt')
  await page.getByRole('button', { name: 'Verify selected contents', exact: true }).click()
  await page.getByText('Selected file contents match (SHA-256).', { exact: true }).waitFor()
  check('matching selected contents are independently hashed', true)
  await choose('Right', a)
  await select(left, 'twins.txt')
  check('same-folder transfer is disabled', await left.getByRole('button', { name: 'Copy to right…', exact: true }).isDisabled())
  await choose('Right', outside)
  const refused = { cells: await right.getByRole('gridcell').count(), text: await right.innerText(), status: await right.getByRole('status').innerText() }
  assert.ok(refused.cells === 0 && !refused.text.includes('outside canary.txt') && refused.status.includes('outside the currently included'), JSON.stringify(refused))
  check('an ungranted address clears previous rows and exposes no outside data', true)
  await choose('Right', b)
  for (const [width, height] of [[1440, 900], [960, 600], [720, 500], [390, 740]]) {
    await page.setViewportSize({ width, height }); await page.waitForTimeout(120)
    const size = await page.evaluate(() => ({ width: innerWidth, height: innerHeight, scrollWidth: document.documentElement.scrollWidth, scrollHeight: document.documentElement.scrollHeight, panes: [...document.querySelectorAll('.fm-pane')].filter(el => getComputedStyle(el).display !== 'none').map(el => ({ width: el.clientWidth, height: el.clientHeight, listHeight: el.querySelector('.fm-list').clientHeight })) }))
    await page.screenshot({ path: join(output, `file-manager-${width}.png`) })
    assert.ok(size.scrollWidth <= width + 1 && size.scrollHeight <= height + 1 && size.panes.length === (width <= 800 || height <= 640 ? 1 : 2) && size.panes.every(pane => pane.width > 200 && pane.height > 120 && pane.listHeight >= 80), JSON.stringify(size))
    check(`panes remain contained with usable file rows at ${width}x${height}`, true)
    if (width <= 800 || height <= 640) {
      await page.getByRole('button', { name: 'Right pane', exact: true }).click()
      check(`compact pane switch preserves the other folder at ${width}x${height}`, await right.getByRole('grid').isVisible() && await right.getByRole('grid').getAttribute('data-folder') === b)
      await page.getByRole('button', { name: 'Left pane', exact: true }).click()
    }
    await page.screenshot({ path: join(output, `file-manager-${width}.png`) })
  }
  check('native UI reports no page or console errors', ui.errors.length === 0)
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, errors: ui.errors }, null, 2))
  console.log(JSON.stringify({ passed: checks.length, output }))
} finally {
  if (ui) await ui.close()
  if (h) await h.stop()
}

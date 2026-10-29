// Alpha0.1: native paged search, durable operations, archive publication/recovery and optional
// live Everything provider. Only this script's own scratch profile/folder/index is used.
import assert from 'node:assert/strict'
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { spawn } from 'node:child_process'
import { startHarness, until, sleep } from '../harness.mjs'
import { openUi } from '../ui.mjs'

const output = process.env.ZEUS_ALPHA_FILES_EVIDENCE
if (!output) throw new Error('ZEUS_ALPHA_FILES_EVIDENCE must name a private scratch directory')
const home = resolve(join(output, 'profile'))
const helper = process.env.ZEUS_EVERYTHING_BROKER
const reference = process.env.ZEUS_EVERYTHING_REFERENCE
if (existsSync(home)) rmSync(home, { recursive: true, force: true })
mkdirSync(home, { recursive: true })
const read = join(home, 'read scope')
const managed = join(home, 'managed')
const outside = join(home, 'outside')
for (const dir of [read, managed, outside]) mkdirSync(dir, { recursive: true })
for (let n = 0; n < 1006; n++) writeFileSync(join(read, `fixture-${String(n).padStart(4, '0')}.txt`), `fixture${n}`)
writeFileSync(join(read, '-exit 資料 canary.txt'), 'same')
writeFileSync(join(read, 'second canary.txt'), 'same')
writeFileSync(join(outside, 'outside canary.txt'), 'outside')
const checks = []
const check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h, ui, instanceProcess
try {
  h = await startHarness({ home })
  await h.ready()
  const cfg = await h.get('/native/v1/index/config')
  cfg.roots.push(...[read, managed].map(path => ({ path, include: true, monitor_changes: true, rescan_every_hours: null, download: false, manage_files: path === managed })))
  await h.put('/native/v1/index/config', cfg)
  await h.post('/svc/v1/index/rescan')
  const first = await until(async () => {
    const page = await h.post('/native/v1/index/search', { groups: [], sort: 'name', limit: 500 })
    return page.total >= 1008 && page
  }, { timeoutMs: 60000, what: 'scoped index snapshot' })
  check('native search returns one bounded500-row page with complete count', first.files.length === 500 && first.total >= 1008)
  const scoped = await h.post('/native/v1/index/search', { groups: [[{ kind: 'text', value: 'canary', path: false, match_case: false, whole_word: false, neg: false }]], sort: 'name', limit: 500 })
  check('search omits the ungranted outside path', scoped.total === 2 && scoped.files.every(f => f.path.startsWith(read)))
  const duplicate = await h.post('/native/v1/files/duplicates/verify', { paths: scoped.files.map(f => f.path) })
  check('duplicate verification reads identical selected bytes without deletion', duplicate.groups.length === 1 && duplicate.groups[0].paths.length === 2)
  const from = join(managed, 'before.txt')
  writeFileSync(from, 'move fixture')
  const moved = await h.post('/native/v1/files/rename', { path: from, name: 'after.txt' })
  check('rename returns a durable undo receipt', moved.can_undo && existsSync(join(managed, 'after.txt')))
  await h.post('/native/v1/files/history/undo', { id: moved.id })
  check('guarded undo restores the original path and content', readFileSync(from, 'utf8') === 'move fixture' && !existsSync(join(managed, 'after.txt')))
  const plan = await h.post('/native/v1/files/archive/prepare', { kind: 'create', source: from, directory: managed, name: 'bundle.zip' })
  await h.post('/native/v1/files/archive/start', { id: plan.id })
  const complete = await until(async () => {
    const jobs = (await h.get('/native/v1/files/archives')).jobs
    const job = jobs.find(j => j.id === plan.id)
    return job && !['running', 'publishing'].includes(job.state) && job
  }, { timeoutMs: 60000, what: 'ZIP creation' })
  check('native ZIP job publishes a SHA256-receipted output', complete.state === 'complete' && complete.sha256?.length === 64 && existsSync(join(managed, 'bundle.zip')))
  const unpack = await h.post('/native/v1/files/archive/prepare', { kind: 'extract', source: join(managed, 'bundle.zip'), directory: managed, name: 'unpacked' })
  await h.post('/native/v1/files/archive/start', { id: unpack.id })
  await until(async () => {
    const job = (await h.get('/native/v1/files/archives')).jobs.find(j => j.id === unpack.id)
    if (job && ['failed', 'uncertain'].includes(job.state)) throw new Error(job.error)
    return job?.state === 'complete'
  }, { timeoutMs: 60000, what: 'ZIP extraction' })
  check('extraction preserves exact content in a new destination', readFileSync(join(managed, 'unpacked', 'before.txt'), 'utf8') === 'move fixture')

  if (process.platform === 'win32' && helper && reference) {
    // Inspect is read-only. Preprovision only this test profile from our known source-built broker;
    // no actual user's trust surface or settings are clicked or changed by the test.
    const pin = await h.post('/native/v1/index/everything/inspect', { path: helper })
    await h.stop(); h = undefined
    const stateDir = join(home, 'app', 'index', 'everything')
    mkdirSync(stateDir, { recursive: true })
    writeFileSync(join(stateDir, 'settings.json'), JSON.stringify({ enabled: true, helper: pin.helper, sha256: pin.sha256, instance: 'ZeusAlphaFixtureUi', file_id: pin.file_id }))
    const app = join(output, 'everything-fixture')
    mkdirSync(app, { recursive: true })
    const exe = join(app, 'Everything64.exe')
    copyFileSync(reference, exe)
    const ini = `[Everything]\napp_data=0\nalpha_instance=0\nrun_as_admin=0\nservice=0\ncheck_for_updates_on_startup=0\nshow_tray_icon=0\nrun_in_background=1\nsearch_history_enabled=0\nrun_history_enabled=0\nhttp_server_enabled=0\netp_server_enabled=0\nauto_include_fixed_volumes=0\nauto_include_removable_volumes=0\nauto_include_fixed_refs_volumes=0\nauto_include_removable_refs_volumes=0\nauto_include_fixed_fat_volumes=0\nauto_include_removable_fat_volumes=0\nauto_include_remote_volumes=0\nntfs_volume_guids=\nntfs_volume_paths=\nntfs_volume_includes=\nrefs_volume_guids=\nrefs_volume_paths=\nrefs_volume_includes=\nfolders=${read}\nfolder_monitor_changes=1\n`
    for (const name of ['fixture.ini', 'Everything.ini', 'Everything-1.5a.ini']) writeFileSync(join(app, name), ini)
    instanceProcess = spawn(exe, ['-instance', 'ZeusAlphaFixtureUi', '-config', join(app, 'fixture.ini'), '-no-db', '-startup'], {
      cwd: app, env: { SystemRoot: process.env.SystemRoot, WINDIR: process.env.WINDIR, APPDATA: app, LOCALAPPDATA: app, TEMP: app, TMP: app }, windowsHide: true, stdio: 'ignore',
    })
    h = await startHarness({ home }); await h.ready()
    const provider = await until(async () => {
      try { const page = await h.post('/native/v1/index/everything/search', { root: read, text: 'canary', sort: 'name', descending: false, offset: 0 }); return page.files.length === 2 && page }
      catch (e) { if (instanceProcess.exitCode !== null) throw e; return false }
    }, { timeoutMs: 30000, everyMs: 300, what: 'fixture-only SDK provider' })
    check('production Rust adapter confines the actual SDK results', provider.files.every(f => f.path.startsWith(read)) && provider.provider === 'everything_ipc2')
  }

  ui = await openUi(h)
  await ui.page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/files`)
  await ui.page.getByRole('button', { name: 'Storage and history', exact: true }).click()
  await ui.page.getByRole('button', { name: 'Move history', exact: true }).click()
  check('native file-operation history is visible in the real UI', await ui.page.getByText('undone', { exact: false }).count() > 0)
  if (instanceProcess) {
    await ui.page.getByRole('button', { name: 'Everything', exact: true }).click()
    await ui.page.getByLabel('Included folder', { exact: true }).selectOption(read)
    await ui.page.getByLabel('Literal filename text', { exact: true }).fill('canary')
    await ui.page.getByRole('button', { name: 'Search Everything', exact: true }).click()
    await ui.page.getByRole('status').filter({ hasText: /2 results loaded/ }).waitFor()
    check('the real UI shows actual scoped Everything Unicode results', (await ui.page.locator('.everything-results .file-insight-row > span.mono').allTextContents()).filter(text => text.startsWith('-exit 資料 canary.txt')).length === 1)
    await ui.page.screenshot({ path: join(output, 'everything-provider.png'), fullPage: true })
  }
  check('alpha file workspace has no page errors', ui.errors.length === 0)
  writeFileSync(join(output, 'results.json'), JSON.stringify({ passed: true, checks, pageErrors: ui.errors, privateProfile: home, providerAcceptance: !!instanceProcess, broker: helper ?? null }, null, 2))
} catch (error) {
  if (ui) {
    writeFileSync(join(output, 'failure.json'), JSON.stringify({ error: String(error), body: await ui.page.locator('body').innerText(), pageErrors: ui.errors }, null, 2))
    await ui.page.screenshot({ path: join(output, 'failure.png'), fullPage: true })
  }
  throw error
} finally {
  await ui?.close()
  await h?.stop()
  if (instanceProcess && instanceProcess.exitCode === null) {
    instanceProcess.kill()
    await until(() => instanceProcess.exitCode !== null, { timeoutMs: 10000, everyMs: 50, what: 'owned Everything fixture shutdown' }).catch(() => undefined)
  }
}

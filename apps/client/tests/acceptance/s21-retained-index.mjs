// Actual native index/listing routes; every path belongs to a fresh scratch profile.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'

if (!process.env.ZEUS_INDEX_EVIDENCE) throw new Error('Set ZEUS_INDEX_EVIDENCE to a new scratch directory')
const output = resolve(process.env.ZEUS_INDEX_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a new scratch profile')
const root = join(home, 'indexed'), child = join(root, 'child'), outside = join(home, 'outside')
for (const p of [child, outside]) mkdirSync(p, { recursive: true })
writeFileSync(join(child, 'kept.txt'), 'inside')
writeFileSync(join(root, '資料.txt'), 'Unicode')
writeFileSync(join(root, 'excluded.tmp'), 'excluded')
writeFileSync(join(outside, 'outside-canary.txt'), 'untouched')
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h
try {
  h = await startHarness({ home }); await h.ready()
  const config = await h.get('/native/v1/index/config')
  config.roots.push({ path: root, include: true, monitor_changes: true, rescan_every_hours: null, download: false, manage_files: false })
  config.exclusions.push('*.tmp')
  await h.put('/native/v1/index/config', config)
  const rows = async () => (await h.get('/svc/v1/index')).files ?? []
  await until(async () => (await rows()).some(r => r.path === join(child, 'kept.txt')))
  check('native scan indexes scoped Unicode and nested files', (await rows()).some(r => r.path === join(root, '資料.txt')))
  check('native scan preserves configured exclusions', !(await rows()).some(r => r.path.endsWith('excluded.tmp')))
  const list = (path, after = '', limit = 2) => h.post('/native/v1/files/list', { path, after, limit })
  const first = await list(root), second = await list(root, first.next)
  check('live listings page without excluding user-visible files', first.files.length === 2 && !!first.next && second.files.length === 1 && second.next === null && [...first.files, ...second.files].some(r => r.path.endsWith('excluded.tmp')))
  writeFileSync(join(child, 'new.txt'), 'new')
  await until(async () => (await rows()).some(r => r.path === join(child, 'new.txt')))
  check('watcher updates use the same enabled scope', true)
  rmSync(join(child, 'new.txt'))
  await until(async () => !(await rows()).some(r => r.path === join(child, 'new.txt')))
  check('confirmed missing leaves are removed from the index', true)
  renameSync(child, join(root, 'original-child'))
  symlinkSync(outside, child, process.platform === 'win32' ? 'junction' : 'dir')
  const refused = async (name, run) => { let denied = false; try { await run() } catch (e) { denied = e.status >= 400 && !!e.code } check(name, denied) }
  await refused('live listing refuses a replaced child junction', () => list(child))
  const visible = await list(root, '', 100)
  check('parent listing omits junctions and reports a skipped entry', !visible.files.some(r => r.path === child) && visible.skipped >= 1)
  await h.post('/svc/v1/index/rescan')
  await until(async () => (await h.get('/native/v1/index/status')).state === 'current')
  check('full rescan never indexes the outside junction target', !(await rows()).some(r => r.path.includes('outside-canary')))
  await refused('ungranted folders cannot be listed', () => list(outside))
  rmSync(child)
  renameSync(root, join(home, 'old-scope')); mkdirSync(root)
  writeFileSync(join(root, 'foreign.txt'), 'foreign')
  await refused('a replaced enabled root cannot regain listing authority', () => list(root))
  await h.post('/svc/v1/index/rescan')
  await until(async () => (await h.get('/native/v1/index/status')).state === 'current')
  check('replacement data is not admitted to the index', !(await rows()).some(r => r.path.endsWith('foreign.txt')))
  check('outside and original file contents are unchanged', readFileSync(join(outside, 'outside-canary.txt'), 'utf8') === 'untouched' && readFileSync(join(home, 'old-scope', 'original-child', 'kept.txt'), 'utf8') === 'inside')
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, platform: process.platform }, null, 2))
} finally { await h?.stop() }

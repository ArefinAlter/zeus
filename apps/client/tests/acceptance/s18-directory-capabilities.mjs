// Actual native routes in a new private profile. Never opens an existing user's files or roots.
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness, until } from '../harness.mjs'

if (!process.env.ZEUS_CAPABILITIES_EVIDENCE) throw new Error('Set ZEUS_CAPABILITIES_EVIDENCE to a new scratch directory')
const output = resolve(process.env.ZEUS_CAPABILITIES_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a new scratch profile')
const reads = join(home, 'read scope'), writes = join(home, 'managed'), outside = join(home, 'outside')
for (const path of [reads, writes, outside]) mkdirSync(path, { recursive: true })
writeFileSync(join(reads, '資料 source.txt'), 'original bytes')
mkdirSync(join(reads, 'tree', 'inner'), { recursive: true })
writeFileSync(join(reads, 'tree', 'inner', '.hidden'), 'nested bytes')
writeFileSync(join(outside, 'victim'), 'outside unchanged')
const checks = []
const check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h
try {
  h = await startHarness({ home }); await h.ready()
  const cfg = await h.get('/native/v1/index/config')
  cfg.roots.push(...[reads, writes].map(path => ({ path, include: true, monitor_changes: false, rescan_every_hours: null, download: false, manage_files: path === writes })))
  await h.put('/native/v1/index/config', cfg)
  const refuse = async (name, route, body, expectedCode) => {
    let error
    try { await h.post(route, body) } catch (e) { error = e }
    check(name, !!error && error.status >= 400 && !!error.code && (!expectedCode || error.code === expectedCode))
  }
  const waitCopy = async (id) => until(async () => {
    const job = await h.post('/native/v1/files/copy/status', { id })
    return !['scanning', 'copying'].includes(job.state) && job
  }, { what: 'native background copy' })
  await h.post('/native/v1/files/copy', { path: join(reads, '資料 source.txt'), dir: writes })
  check('read-only source copies exact Unicode-named bytes', readFileSync(join(writes, '資料 source.txt'), 'utf8') === 'original bytes')
  await refuse('existing destination is never replaced', '/native/v1/files/copy', { path: join(reads, '資料 source.txt'), dir: writes })
  await refuse('read scope cannot become a write scope', '/native/v1/files/mkdir', { parent: reads, name: 'forbidden' })
  await refuse('read-only source cannot be moved', '/native/v1/files/move', { path: join(reads, '資料 source.txt'), dir: writes })
  const folder = await h.post('/native/v1/files/mkdir', { parent: writes, name: 'new folder' })
  check('managed folder creation succeeds', existsSync(folder.path))
  await refuse('ADS names are refused', '/native/v1/files/mkdir', { parent: writes, name: 'file:stream' })
  await refuse('device aliases are refused', '/native/v1/files/mkdir', { parent: writes, name: 'COM¹.txt' })
  const copy = await h.post('/native/v1/files/copy/start', { path: join(reads, 'tree'), dir: writes })
  const copied = await waitCopy(copy.id)
  check('background recursive copy keeps hidden contents and byte totals', copied.state === 'complete' && copied.bytes === 12 && copied.files === 1 && readFileSync(join(writes, 'tree', 'inner', '.hidden'), 'utf8') === 'nested bytes')
  await refuse('recursive copy rejects an occupied destination during preflight', '/native/v1/files/copy/start', { path: join(reads, 'tree'), dir: writes }, 'app.exists')
  check('refused copy leaves existing destination bytes unchanged', readFileSync(join(writes, 'tree', 'inner', '.hidden'), 'utf8') === 'nested bytes')
  const moved = await h.post('/native/v1/files/rename', { path: join(writes, 'tree'), name: 'renamed tree' })
  check('directory rename has an identity-backed durable receipt', moved.can_undo && readFileSync(join(writes, 'renamed tree', 'inner', '.hidden'), 'utf8') === 'nested bytes')
  await h.post('/native/v1/files/history/undo', { id: moved.id })
  check('guarded directory undo restores exact contents', !existsSync(join(writes, 'renamed tree')) && readFileSync(join(writes, 'tree', 'inner', '.hidden'), 'utf8') === 'nested bytes')
  const link = join(writes, 'junction')
  symlinkSync(outside, link, process.platform === 'win32' ? 'junction' : 'dir')
  await refuse('copy destination cannot traverse a junction', '/native/v1/files/copy', { path: join(reads, '資料 source.txt'), dir: link })
  await refuse('copy source cannot traverse a junction', '/native/v1/files/copy', { path: join(link, 'victim'), dir: writes })
  await refuse('folder creation cannot traverse a junction', '/native/v1/files/mkdir', { parent: link, name: 'forbidden' })
  await refuse('a final junction cannot be renamed as a real folder', '/native/v1/files/rename', { path: link, name: 'moved link' })
  const treeLink = join(reads, 'tree', 'linked outside')
  symlinkSync(outside, treeLink, process.platform === 'win32' ? 'junction' : 'dir')
  await refuse('recursive preflight refuses a linked child', '/native/v1/files/copy/start', { path: join(reads, 'tree'), dir: folder.path })
  check('refused linked tree produces no output', !existsSync(join(folder.path, 'tree')))
  rmSync(treeLink); rmSync(link)
  // No mutation is active: replace the enabled read root after its identity was captured by Settings.
  renameSync(reads, join(home, 'old read scope')); mkdirSync(reads)
  writeFileSync(join(reads, 'replacement'), 'foreign root')
  await refuse('replaced enabled root cannot silently regain read authority', '/native/v1/files/copy', { path: join(reads, 'replacement'), dir: writes })
  check('outside contents and source originals remain untouched', readFileSync(join(outside, 'victim'), 'utf8') === 'outside unchanged' && !existsSync(join(outside, 'forbidden')) && readFileSync(join(home, 'old read scope', '資料 source.txt'), 'utf8') === 'original bytes')
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, platform: process.platform }, null, 2))
} finally { await h?.stop() }

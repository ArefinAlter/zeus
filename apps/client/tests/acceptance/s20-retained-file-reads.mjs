// Retained preview and duplicate-verification routes against a fresh, isolated native profile.
import assert from 'node:assert/strict'
import { existsSync, linkSync, mkdirSync, readFileSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { startHarness } from '../harness.mjs'

if (!process.env.ZEUS_READS_EVIDENCE) throw new Error('Set ZEUS_READS_EVIDENCE to a new scratch directory')
const output = resolve(process.env.ZEUS_READS_EVIDENCE), home = join(output, 'profile')
if (existsSync(home)) throw new Error('Use a new scratch profile')
const root = join(home, 'read scope'), outside = join(home, 'outside')
for (const path of [root, outside]) mkdirSync(path, { recursive: true })
writeFileSync(join(root, '資料.txt'), 'same bytes\n')
writeFileSync(join(root, 'twin.txt'), 'same bytes\n')
writeFileSync(join(root, 'different.txt'), 'else bytes\n')
linkSync(join(root, '資料.txt'), join(root, 'alias.txt'))
writeFileSync(join(root, 'long.txt'), 'x'.repeat(4 * 1024 * 1024 + 1))
writeFileSync(join(root, 'vector.svg'), '<svg><script>throw new Error("not executed")</script></svg>')
writeFileSync(join(root, 'pixel.png'), Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9ioAAAAASUVORK5CYII=', 'base64'))
writeFileSync(join(outside, 'canary.txt'), 'outside unchanged')
const checks = [], check = (name, value) => { assert.ok(value, name); checks.push(name); console.log('PASS ' + name) }
let h
try {
  h = await startHarness({ home }); await h.ready()
  const config = await h.get('/native/v1/index/config')
  config.roots.push({ path: root, include: true, monitor_changes: false, rescan_every_hours: null, download: false, manage_files: false })
  await h.put('/native/v1/index/config', config)
  const preview = path => h.post('/native/v1/files/preview', { path })
  const verify = paths => h.post('/native/v1/files/duplicates/verify', { paths })
  const refused = async (name, action) => {
    let failure
    try { const result = await action(); failure = result.kind === 'refused' } catch (error) { failure = error.status >= 400 && !!error.code }
    check(name, failure)
  }
  const text = await preview(join(root, '資料.txt'))
  check('Unicode text preview preserves exact content and line count', text.kind === 'text' && text.text === 'same bytes\n' && text.lines === 1 && !text.truncated)
  const long = await preview(join(root, 'long.txt'))
  check('large text preview is bounded and skips full line counting', long.kind === 'text' && long.text.length === 65536 && long.truncated && long.lines === null)
  const png = await preview(join(root, 'pixel.png'))
  check('image preview returns the opened image bytes and dimensions', png.kind === 'image' && png.width === 1 && png.height === 1 && Buffer.from(png.data_url.split(',')[1], 'base64').equals(readFileSync(join(root, 'pixel.png'))))
  const svg = await preview(join(root, 'vector.svg'))
  check('SVG remains source text', svg.kind === 'text' && !svg.data_url && svg.note.includes('never renders'))
  check('an enabled read-root itself has a folder preview', (await preview(root)).kind === 'folder')
  const twins = await verify(['資料.txt', 'alias.txt', 'twin.txt'].map(name => join(root, name)))
  check('duplicate hashing counts each hard-linked object once', twins.bytes_read === 22 && twins.groups.length === 1 && twins.groups[0].paths.length === 2 && twins.groups[0].verified && !twins.deletes_files)
  check('same-size different bytes are not reported as duplicates', (await verify(['資料.txt', 'different.txt'].map(name => join(root, name)))).groups.length === 0)
  await refused('aliases alone are not two duplicate files', () => verify(['資料.txt', 'alias.txt'].map(name => join(root, name))))
  await refused('ungranted file previews are refused', () => preview(join(outside, 'canary.txt')))
  await refused('ungranted duplicate inputs are refused', () => verify([join(root, '資料.txt'), join(outside, 'canary.txt')]))
  const junction = join(root, 'junction')
  symlinkSync(outside, junction, process.platform === 'win32' ? 'junction' : 'dir')
  await refused('a junction cannot be previewed as a folder', () => preview(junction))
  await refused('preview reads cannot traverse a junction', () => preview(join(junction, 'canary.txt')))
  await refused('duplicate reads cannot traverse a junction', () => verify([join(root, '資料.txt'), join(junction, 'canary.txt')]))
  rmSync(junction)
  renameSync(root, join(home, 'original scope')); mkdirSync(root)
  writeFileSync(join(root, 'foreign.txt'), 'foreign root')
  await refused('replaced enabled root cannot regain preview authority', () => preview(join(root, 'foreign.txt')))
  await refused('replaced enabled root cannot regain hashing authority', () => verify([join(root, 'foreign.txt'), join(root, 'foreign.txt')]))
  check('all original and outside bytes remain untouched', readFileSync(join(home, 'original scope', '資料.txt'), 'utf8') === 'same bytes\n' && readFileSync(join(outside, 'canary.txt'), 'utf8') === 'outside unchanged')
  writeFileSync(join(output, 'result.json'), JSON.stringify({ checks, platform: process.platform }, null, 2))
} finally { await h?.stop() }

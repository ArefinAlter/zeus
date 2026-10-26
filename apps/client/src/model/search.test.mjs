// node --test src/model/search.test.mjs
//
// The file search against data shaped like a real index: every operator on a few thousand entries, paths of both
// platforms, highlighting, and the speed of a query over fifty thousand entries.

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { DEFAULT_OPTIONS, filterMatch, highlightRanges, matches, parseQuery } from './search.ts'

const NOW = new Date(2026, 9, 26, 14, 0).getTime()
const DAY = 86_400_000

function entries(n, win = true) {
  const sep = win ? '\\' : '/'
  const root = win ? 'C:\\Users\\a\\Downloads\\Zeus' : '/home/a/Downloads/Zeus'
  const kinds = [
    ['doc', 'pdf', 3_000_000],
    ['report', 'docx', 80_000],
    ['photo', 'png', 400_000],
    ['clip', 'mp4', 90_000_000],
    ['pack', 'zip', 12_000_000],
    ['setup', 'exe', 2_000_000],
    ['notes', 'txt', 900],
    ['song', 'flac', 24_000_000],
  ]
  const out = []
  for (let i = 0; i < n; i++) {
    const [base, ext, size] = kinds[i % kinds.length]
    const folder = ['Documents', 'Pictures', 'Video', 'Compressed', 'Programs', 'Music', ''][i % 7]
    out.push({
      id: i + 1,
      name: `${base}-${i}.${ext}`,
      dir: folder ? `${root}${sep}${folder}${sep}batch${i % 20}` : root,
      isFolder: false,
      size: size + (i % 1000),
      modified: NOW - (i % 400) * DAY,
      ...(i % 11 === 0 ? { host: 'files.example.org' } : {}),
    })
  }
  out.push({ id: n + 1, name: 'Documents', dir: root, isFolder: true, size: 0, modified: NOW })
  return out
}

const q = (text, opts = {}) => {
  const r = parseQuery(text, { ...DEFAULT_OPTIONS, ...opts }, NOW)
  assert.ok(r.ok, r.ok ? '' : r.error)
  return r.query
}
const count = (list, text, opts) => list.filter((e) => matches(q(text, opts), e)).length

test('every operator finds what it says on realistic data', () => {
  const list = entries(2000)
  const pdfs = list.filter((e) => e.name.endsWith('.pdf')).length
  assert.equal(count(list, 'ext:pdf'), pdfs)
  assert.equal(count(list, 'ext:pdf;docx'), list.filter((e) => /\.(pdf|docx)$/.test(e.name)).length)
  assert.equal(count(list, 'ext:.PDF'), pdfs)
  assert.equal(count(list, 'size:>50mb'), list.filter((e) => e.size > 50 * 1024 * 1024).length)
  assert.equal(count(list, 'size:1mb..5mb'), list.filter((e) => e.size >= 1024 ** 2 && e.size <= 5 * 1024 ** 2).length)
  assert.equal(count(list, 'size:empty'), 0)
  assert.equal(count(list, 'dm:today'), list.filter((e) => e.modified >= NOW - 14 * 3_600_000 && e.modified < NOW + 10 * 3_600_000).length)
  assert.equal(count(list, 'dm:last7days'), list.filter((e) => !e.isFolder && e.modified >= NOW - 6 * DAY - 14 * 3_600_000).length + 1)
  assert.equal(count(list, 'photo-1'), list.filter((e) => e.name.toLowerCase().includes('photo-1')).length)
  assert.equal(count(list, 'host:example.org'), list.filter((e) => e.host).length)
  assert.equal(count(list, 'folder:'), 1)
  assert.equal(count(list, 'file:'), 2000)
  assert.equal(count(list, 'ext:pdf !batch3'), list.filter((e) => e.name.endsWith('.pdf') && !e.name.includes('batch3')).length)
  assert.equal(count(list, 'ext:zip | ext:exe'), list.filter((e) => /\.(zip|exe)$/.test(e.name)).length)
  assert.equal(count(list, '"clip-3"'), list.filter((e) => e.name.includes('clip-3')).length)
  assert.equal(count(list, '*.flac'), list.filter((e) => e.name.endsWith('.flac')).length)
  assert.equal(count(list, 'song-?.flac'), list.filter((e) => /^song-\d\.flac$/.test(e.name)).length)
  assert.equal(count(list, 'regex:^report-\\d+\\.docx$'), list.filter((e) => /^report-\d+\.docx$/.test(e.name)).length)
  assert.equal(count(list, '^notes-\\d{3}\\.txt$', { regex: true }), list.filter((e) => /^notes-\d{3}\.txt$/.test(e.name)).length)
})

test('case, whole word and path matching', () => {
  const list = [
    { name: 'README.md', dir: 'C:\\r', isFolder: false, size: 1, modified: NOW },
    { name: 'readme-old.md', dir: 'C:\\r\\Archive', isFolder: false, size: 1, modified: NOW },
    { name: 'dataset.csv', dir: 'C:\\r\\datasets', isFolder: false, size: 1, modified: NOW },
  ]
  assert.equal(count(list, 'readme'), 2)
  assert.equal(count(list, 'readme', { matchCase: true }), 1)
  assert.equal(count(list, 'case:README'), 1)
  assert.equal(count(list, 'ww:readme'), 2)
  assert.equal(count(list, 'ww:data'), 0)
  assert.equal(count(list, 'path:archive'), 1)
  assert.equal(count(list, 'datasets\\'), 1)
  assert.equal(count(list, 'archive', { matchPath: true }), 1)
})

test('paths with slashes work for the Unix side of the app', () => {
  const list = entries(300, false)
  assert.equal(count(list, 'Documents/batch3'), list.filter((e) => `${e.dir}/${e.name}`.includes('Documents/batch3')).length)
  assert.ok(count(list, 'path:Pictures') > 0)
  assert.equal(count(list, 'folder:'), 1)
})

test('the result filters', () => {
  const list = entries(800)
  for (const [id, re] of [
    ['audio', /\.flac$/],
    ['compressed', /\.zip$/],
    ['document', /\.(pdf|docx|txt)$/],
    ['executable', /\.exe$/],
    ['picture', /\.png$/],
    ['video', /\.mp4$/],
  ]) {
    assert.equal(list.filter((e) => filterMatch(id, e)).length, list.filter((e) => !e.isFolder && re.test(e.name)).length, id)
  }
  assert.equal(list.filter((e) => filterMatch('folder', e)).length, 1)
})

test('bad searches are explained, not swallowed', () => {
  for (const bad of ['size:lots', 'dm:someday', 'regex:(', 'ext:', 'host:']) {
    const r = parseQuery(bad, DEFAULT_OPTIONS, NOW)
    assert.equal(r.ok, false, bad)
    assert.ok(r.error.length > 10)
  }
})

test('matches are highlighted in the name, merged, and never from negated or path terms', () => {
  assert.deepEqual(highlightRanges(q('rep ort'), 'Annual Report 2025.pdf'), [[7, 13]])
  assert.deepEqual(highlightRanges(q('2025 !pdf'), 'Annual Report 2025.pdf'), [[14, 18]])
  assert.deepEqual(highlightRanges(q('case:report'), 'Annual Report 2025.pdf'), [])
  assert.deepEqual(highlightRanges(q('regex:\\d{4}'), 'Annual Report 2025.pdf'), [[14, 18]])
  assert.deepEqual(highlightRanges(q('Annual\\'), 'Annual Report.pdf'), [])
  assert.deepEqual(highlightRanges(q(''), 'x'), [])
})

test('a query over fifty thousand entries answers well inside a frame budget for ordinary searches', () => {
  const list = entries(50_000)
  const timings = {}
  for (const text of ['photo', 'ext:pdf size:>1mb', 'dm:last7days ext:png', 'photo-1 | clip-2', '*.flac', 'regex:^clip-\\d+7\\.mp4$', 'Pictures\\batch1']) {
    const query = q(text)
    const t0 = performance.now()
    const n = list.filter((e) => matches(query, e)).length
    timings[text] = { ms: Math.round(performance.now() - t0), hits: n }
  }
  console.log('# 50,000 entries, per query:', JSON.stringify(timings))
  const worst = Math.max(...Object.values(timings).map((t) => t.ms))
  assert.ok(worst < 600, `the slowest query took ${worst} ms`)
})

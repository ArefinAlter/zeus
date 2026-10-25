// node --test src/model/*.test.mjs
//
// The pure model behind the screens: search syntax, schedules and the limiter, link intake, categories, approval
// hashes and bind codes, and the agent's closed tool table.

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { ACCESS_ORDER, NEVER, TOOLS, clampSkillTools, mayCall } from './agentTools.ts'
import { allowedScopes, bindCode, canonical, codeMatches, confirmationFor, normalizeCode, requestHash, sha256Hex } from './approvals.ts'
import { categoryOf, defaultMap, extOf, overlaps, parseExtList } from './categories.ts'
import { MAX_URLS, dedupeNames, intake, nameProblem, packagesByHost } from './links.ts'
import { describeSchedule, inWindow, jobRateCap, limiterActive, nextStart, parseHm, startDue, stopAfter } from './schedule.ts'
import { DEFAULT_OPTIONS, dateRange, filterMatch, matches, parseQuery, parseSize, tokenize } from './search.ts'
import { etaSeconds, fmtBytes, fmtEta, parseBytes, percent } from './units.ts'

// ---------------------------------------------------------------- search

const NOW = new Date(2026, 9, 25, 14, 0).getTime() // Sunday 25 October 2026, 14:00 local
const file = (name, size = 1000, modified = NOW, dir = 'D:\\Downloads\\Zeus', extra = {}) => ({ name, size, modified, dir, isFolder: false, ...extra })
const q = (text, opts = {}) => {
  const r = parseQuery(text, { ...DEFAULT_OPTIONS, ...opts }, NOW)
  assert.ok(r.ok, r.ok ? '' : r.error)
  return r.query
}

test('words are AND, | is OR and binds tighter, ! negates, quotes keep spaces', () => {
  assert.deepEqual(tokenize('a "b c" d|e'), ['a', '"b c"', 'd', '|', 'e'])
  const e = file('Annual Report 2025.pdf')
  assert.ok(matches(q('annual report'), e))
  assert.ok(!matches(q('annual budget'), e))
  assert.ok(matches(q('annual budget|report'), e))
  assert.ok(!matches(q('!report'), e))
  assert.ok(matches(q('"report 2025"'), e))
  assert.ok(!matches(q('"report  2025"'), e))
  assert.ok(matches(q(''), e))
})

test('case, whole word and wildcards', () => {
  const e = file('README-dataset.md')
  assert.ok(matches(q('readme'), e))
  assert.ok(!matches(q('readme', { matchCase: true }), e))
  assert.ok(matches(q('case:README'), e))
  assert.ok(!matches(q('ww:data'), e))
  assert.ok(matches(q('ww:dataset'), e))
  assert.ok(matches(q('*.md'), e))
  assert.ok(!matches(q('*.pdf'), e))
  assert.ok(matches(q('readme-??????t.md'), e))
})

test('ext:, size:, dm:, path:, host:, file: and folder:', () => {
  const pdf = file('paper.pdf', 3 * 1024 * 1024, new Date(2026, 9, 21, 9).getTime(), 'D:\\Downloads\\Zeus\\Documents', { host: 'papers.example.org' })
  const iso = file('ubuntu.iso', 6 * 1024 ** 3, new Date(2026, 9, 25, 8).getTime())
  const dir = { name: 'Documents', dir: 'D:\\Downloads\\Zeus', isFolder: true, size: 0, modified: NOW }
  assert.ok(matches(q('ext:pdf;epub'), pdf))
  assert.ok(!matches(q('ext:epub'), pdf))
  assert.ok(matches(q('size:>1mb'), pdf))
  assert.ok(matches(q('size:1mb..5mb'), pdf))
  assert.ok(!matches(q('size:<1mb'), pdf))
  assert.ok(matches(q('size:large'), pdf))
  assert.ok(matches(q('size:gigantic'), iso))
  assert.ok(matches(q('dm:today'), iso))
  assert.ok(!matches(q('dm:today'), pdf))
  assert.ok(matches(q('dm:thisweek'), iso), 'Sunday belongs to the week that started Monday')
  assert.ok(matches(q('dm:last7days'), pdf))
  assert.ok(matches(q('dm:2026-10'), pdf))
  assert.ok(matches(q('dm:>2026-10-20'), pdf))
  assert.ok(!matches(q('dm:<2026-10-21'), pdf))
  assert.ok(matches(q('path:documents'), pdf))
  assert.ok(matches(q('documents\\'), pdf), 'a separator in a term searches the path')
  assert.ok(matches(q('host:example.org'), pdf))
  assert.ok(!matches(q('host:example.com'), pdf))
  assert.ok(matches(q('folder:'), dir))
  assert.ok(!matches(q('file:'), dir))
  assert.ok(matches(q('file: paper'), pdf))
})

test('regex mode and regex: terms; bad input is an error, not a crash', () => {
  const e = file('report-2025-q3.xlsx')
  assert.ok(matches(q('report-\\d{4}', { regex: true }), e))
  assert.ok(matches(q('regex:^report-\\d+'), e))
  assert.equal(parseQuery('(', { ...DEFAULT_OPTIONS, regex: true }, NOW).ok, false)
  assert.equal(parseQuery('regex:(', DEFAULT_OPTIONS, NOW).ok, false)
  assert.equal(parseQuery('size:lots', DEFAULT_OPTIONS, NOW).ok, false)
  assert.equal(parseQuery('dm:someday', DEFAULT_OPTIONS, NOW).ok, false)
})

test('sizes and named dates', () => {
  assert.equal(parseSize('10mb'), 10 * 1024 * 1024)
  assert.equal(parseSize('1.5 GB'), 1.5 * 1024 ** 3)
  assert.equal(parseSize('512'), 512)
  assert.equal(parseSize('3 parsecs'), null)
  const [from, to] = dateRange('yesterday', NOW)
  assert.equal(to - from, 86_400_000)
  assert.equal(new Date(from).getDate(), 24)
})

test('result filters follow the file type, folders only under Folder', () => {
  assert.ok(filterMatch('video', file('talk.mkv')))
  assert.ok(!filterMatch('video', file('talk.mp3')))
  assert.ok(filterMatch('executable', file('setup.exe')))
  assert.ok(filterMatch('folder', { name: 'x', dir: '', isFolder: true, size: 0, modified: 0 }))
  assert.ok(filterMatch('everything', file('anything')))
})

// ---------------------------------------------------------------- schedules and the limiter

const sched = (o = {}) => ({ startEnabled: true, startTime: '23:00', repeat: 'days', startDate: '2026-10-25', days: [0, 1, 2, 3, 4, 5, 6], stopEnabled: true, stopTime: '06:00', ...o })

test('a daily start is today when still ahead, else tomorrow', () => {
  assert.equal(nextStart(sched(), NOW), new Date(2026, 9, 25, 23, 0).getTime())
  const late = new Date(2026, 9, 25, 23, 30).getTime()
  assert.equal(nextStart(sched(), late), new Date(2026, 9, 26, 23, 0).getTime())
})

test('weekday schedules skip the weekend; a one-time start in the past has no next start', () => {
  const weekdays = sched({ days: [1, 2, 3, 4, 5], startTime: '02:30' })
  assert.equal(nextStart(weekdays, NOW), new Date(2026, 9, 26, 2, 30).getTime())
  assert.equal(nextStart(sched({ repeat: 'once', startDate: '2026-10-24' }), NOW), null)
  assert.equal(nextStart(sched({ repeat: 'once', startDate: '2026-10-26', startTime: '01:00' }), NOW), new Date(2026, 9, 26, 1, 0).getTime())
  assert.equal(nextStart(sched({ days: [] }), NOW), null)
  assert.equal(nextStart(sched({ startEnabled: false }), NOW), null)
})

test('a stop earlier than the start falls on the next day; due checks are half-open', () => {
  const start = new Date(2026, 9, 25, 23, 0).getTime()
  assert.equal(stopAfter(sched(), start), new Date(2026, 9, 26, 6, 0).getTime())
  assert.ok(startDue(sched(), start - 1000, start))
  assert.ok(!startDue(sched(), start, start + 1000))
  assert.equal(parseHm('24:00'), null)
  assert.equal(parseHm('7:05'), 425)
})

test('schedule descriptions', () => {
  assert.equal(describeSchedule(sched()), 'Daily at 23:00, stops at 06:00')
  assert.equal(describeSchedule(sched({ days: [1, 2, 3, 4, 5], stopEnabled: false })), 'Weekdays at 23:00')
  assert.equal(describeSchedule(sched({ days: [5, 1, 3], stopEnabled: false })), 'Mon, Wed, Fri at 23:00')
  assert.equal(describeSchedule(sched({ startEnabled: false })), 'Starts by hand')
})

test('limiter windows past midnight belong to their start day', () => {
  const sat = 6
  const lateSat = new Date(2026, 9, 24, 23, 30).getTime()
  const earlySun = new Date(2026, 9, 25, 5, 0).getTime()
  const sunNoon = new Date(2026, 9, 25, 12, 0).getTime()
  assert.ok(inWindow('22:00', '07:00', [sat], lateSat))
  assert.ok(inWindow('22:00', '07:00', [sat], earlySun))
  assert.ok(!inWindow('22:00', '07:00', [sat], sunNoon))
  assert.ok(inWindow('09:00', '17:00', [0], sunNoon))
  const limiter = { enabled: true, limitKiBps: 1024, scheduleEnabled: true, from: '09:00', to: '17:00', days: [1, 2, 3, 4, 5] }
  assert.ok(!limiterActive(limiter, sunNoon), 'a weekday window is off on Sunday')
  assert.ok(limiterActive({ ...limiter, scheduleEnabled: false }, sunNoon))
  assert.ok(!limiterActive({ ...limiter, enabled: false }, sunNoon))
})

test('a job gets its share of the tighter of the global and queue limits', () => {
  const limiter = { enabled: true, limitKiBps: 1000, scheduleEnabled: false, from: '00:00', to: '00:00', days: [] }
  const base = { capacity: 10_000_000, limiter, now: NOW, activeJobs: 4, queueLimitKiBps: null, queueActiveJobs: 0 }
  assert.equal(jobRateCap(base), 256_000)
  assert.equal(jobRateCap({ ...base, queueLimitKiBps: 200, queueActiveJobs: 2 }), 102_400)
  assert.equal(jobRateCap({ ...base, limiter: { ...limiter, enabled: false } }), 10_000_000)
})

// ---------------------------------------------------------------- link intake

test('intake takes http(s) only, drops exact duplicates, keeps different query strings apart', () => {
  const text = [
    'https://files.example.org/a.zip',
    'https://files.example.org/a.zip',
    'https://files.example.org/a.zip?sig=1',
    'https://files.example.org/a.zip?sig=2',
    'ftp://old.example.net/b.iso',
    'magnet:?xt=urn:btih:abc',
    'https://user:pw@files.example.org/c.pdf',
    'see http://mirror.example.com/d.tar.gz, thanks',
  ].join('\n')
  const r = intake(text)
  assert.equal(r.links.length, 4)
  assert.equal(r.duplicates, 1)
  assert.equal(r.refused.length, 3)
  assert.ok(r.links.every((l) => !l.display.includes('sig=')), 'query strings are withheld from display')
  assert.equal(r.links.filter((l) => l.queryWithheld).length, 2)
  assert.equal(r.links[3].url, 'http://mirror.example.com/d.tar.gz')
})

test('intake reads links out of a pasted page and stops at the limit', () => {
  const page = '<a href="https://cdn.example.org/x/report%20one.pdf">one</a> <a href=\'/relative\'>no</a> <img src="https://cdn.example.org/i.png">'
  const r = intake(page)
  assert.deepEqual(r.links.map((l) => l.name), ['report one.pdf', 'i.png'])
  const many = Array.from({ length: MAX_URLS + 7 }, (_, i) => `https://h.example.org/f${i}.bin`).join('\n')
  const big = intake(many)
  assert.equal(big.links.length, MAX_URLS)
  assert.equal(big.overLimit, 7)
})

test('packages by host, collision-free names and refused names', () => {
  const pk = packagesByHost([{ host: 'b.example' }, { host: 'a.example' }, { host: 'b.example' }])
  assert.deepEqual(pk.map((p) => [p.host, p.items.length]), [['b.example', 2], ['a.example', 1]])
  assert.deepEqual(dedupeNames(['a.zip', 'A.zip', 'b.zip', 'a.zip']), ['a.zip', 'A (1).zip', 'b.zip', 'a (2).zip'])
  assert.equal(nameProblem('ok name.pdf'), null)
  assert.ok(nameProblem('..'))
  assert.ok(nameProblem('a/b.pdf'))
  assert.ok(nameProblem('CON.txt'))
  assert.ok(nameProblem('trailing.'))
})

// ---------------------------------------------------------------- categories

test('categories by extension, editable lists and overlap detection', () => {
  assert.equal(extOf('archive.tar.gz'), 'gz')
  assert.equal(extOf('.hidden'), '')
  assert.equal(categoryOf('Movie.MKV'), 'video')
  assert.equal(categoryOf('setup.exe'), 'programs')
  assert.equal(categoryOf('notes'), 'general')
  assert.deepEqual(parseExtList('zip, .RAR *.7z zip'), ['zip', 'rar', '7z'])
  const map = defaultMap()
  assert.deepEqual(overlaps(map), [])
  map.documents.push('zip')
  assert.deepEqual(overlaps(map), [{ ext: 'zip', categories: ['compressed', 'documents'] }])
})

// ---------------------------------------------------------------- approvals

test('SHA-256 matches known vectors', () => {
  assert.equal(sha256Hex(''), 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855')
  assert.equal(sha256Hex('abc'), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')
  assert.equal(sha256Hex('a'.repeat(1000)), '41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3')
})

test('canonical facts sort keys, so the hash does not depend on member order', () => {
  assert.equal(canonical({ b: 1, a: [true, null, 'x'] }), '{"a":[true,null,"x"],"b":1}')
  assert.equal(requestHash({ a: 1, b: 2 }), requestHash({ b: 2, a: 1 }))
  assert.notEqual(requestHash({ a: 1, b: 2 }), requestHash({ a: 1, b: 3 }))
  assert.throws(() => canonical({ x: 0.5 }))
})

test('bind codes are 30 bits in Crockford base 32 and read forgivingly', () => {
  const code = bindCode('ffffffff00')
  assert.equal(code, 'ZZZ-ZZZ')
  assert.equal(bindCode('00000000'), '000-000')
  assert.match(bindCode(requestHash({ x: 1 })), /^[0-9A-HJKMNP-TV-Z]{3}-[0-9A-HJKMNP-TV-Z]{3}$/)
  assert.equal(normalizeCode('wpb-zdo'), 'WPBZD0')
  assert.ok(codeMatches('1lI-o0O', '111-000'))
  assert.ok(!codeMatches('111-001', '111-000'))
})

test('scopes are computed from the request, and the stronger confirmations', () => {
  assert.deepEqual(allowedScopes('session_grant', { coversJobs: true, namesRun: false }), ['once', 'site', 'never'])
  assert.deepEqual(allowedScopes('session_grant', { coversJobs: false, namesRun: true }), ['site', 'workflow', 'never'])
  assert.deepEqual(allowedScopes('destructive_op', { coversJobs: true, namesRun: false }), [])
  assert.equal(confirmationFor('session_grant', 'site'), 'gesture')
  assert.equal(confirmationFor('session_grant', 'workflow'), 'typed_code')
  assert.equal(confirmationFor('cloud_delivery'), 'typed_code')
})

// ---------------------------------------------------------------- the agent's tools

test('the tool table is the thirteen MCP tools and none decides, reads a secret or runs a shell', () => {
  assert.equal(TOOLS.length, 13)
  assert.deepEqual(
    TOOLS.map((t) => t.name).sort(),
    ['zeus_artifacts', 'zeus_cloud', 'zeus_control', 'zeus_enqueue', 'zeus_files', 'zeus_files_list', 'zeus_probe', 'zeus_session_list', 'zeus_session_request', 'zeus_status', 'zeus_verify', 'zeus_wait', 'zeus_workflow'],
  )
  for (const t of TOOLS) assert.doesNotMatch(t.name, /approve|decide|grant|cookie|secret|shell|exec|open_file/i)
  assert.ok(NEVER.some((n) => /approval/.test(n)))
})

test('access levels gate tools and actions; deciding words are refused at every level', () => {
  const all = TOOLS.map((t) => t.name)
  assert.ok(mayCall('zeus_probe', 'read', all).ok)
  const r = mayCall('zeus_enqueue', 'read', all)
  assert.equal(r.ok, false)
  assert.match(r.reason, /needs Download access/)
  assert.ok(mayCall('zeus_enqueue', 'download', all).ok)
  assert.equal(mayCall('zeus_cloud', 'download', all).ok, false)
  assert.ok(mayCall('zeus_cloud', 'organize', all).ok)
  assert.ok(mayCall('zeus_artifacts', 'read', all, 'list').ok)
  assert.equal(mayCall('zeus_artifacts', 'read', all, 'ingest').ok, false)
  for (const access of ACCESS_ORDER) {
    const d = mayCall('zeus_session_request', access, all, 'approve')
    assert.equal(d.ok, false)
    assert.match(d.reason, /only a person/)
  }
  assert.equal(mayCall('zeus_probe', 'organize', all.filter((n) => n !== 'zeus_probe')).ok, false)
  assert.equal(mayCall('zeus_approve', 'organize', all).ok, false)
})

test('a skill cannot add a tool outside the table', () => {
  assert.deepEqual(clampSkillTools(['zeus_probe', 'zeus_approve', 'shell', 'zeus_probe', 'zeus_enqueue']), ['zeus_probe', 'zeus_enqueue'])
})

// ---------------------------------------------------------------- units

test('sizes, rates and times', () => {
  assert.equal(fmtBytes(0), '0 B')
  assert.equal(fmtBytes(1536), '1.50 KiB')
  assert.equal(fmtBytes(5 * 1024 ** 3), '5.00 GiB')
  assert.equal(fmtEta(59), '59 s')
  assert.equal(fmtEta(3725), '1 h 02 min')
  assert.equal(fmtEta(null), '–')
  assert.equal(etaSeconds(1000, 0), null)
  assert.equal(etaSeconds(1000, 100), 10)
  assert.equal(percent(50, 200), 25)
  assert.equal(percent(5, null), null)
  assert.equal(parseBytes('1.5 MiB'), 1572864)
  assert.equal(parseBytes('200k'), 204800)
  assert.equal(parseBytes('lots'), null)
})

// node --test src/model/grab.test.mjs
//
// The link grabber's rules: duplicate detection, what an online check means, folders for categories.

import assert from 'node:assert/strict'
import { test } from 'node:test'

import { checkNote, duplicateWhere, findDuplicate, folderFor, folderProblem, insideRoot, overwriteFor, urlKey } from './grab.ts'

test('two addresses are the same file when scheme, host and path agree, whatever the query', () => {
  assert.equal(urlKey('https://Files.Example.org:443/a/b.zip?token=1'), 'https://files.example.org/a/b.zip')
  assert.equal(urlKey('http://127.0.0.1:18999/files/x.bin?size=5&seed=2'), 'http://127.0.0.1:18999/files/x.bin')
  assert.equal(urlKey('http://example.org/a/b/'), 'http://example.org/a/b')
  assert.equal(urlKey('http://example.org/'), 'http://example.org/')
  assert.equal(urlKey('http://example.org/a.zip?…'), 'http://example.org/a.zip')
  assert.notEqual(urlKey('http://example.org/a.zip'), urlKey('https://example.org/a.zip'))
})

test('a failed or canceled download is not a duplicate; a finished or running one is', () => {
  const existing = [
    { id: 'j1', url: 'http://example.org/a.zip', name: 'a.zip', state: 'failed' },
    { id: 'j2', url: 'http://example.org/b.zip', name: 'b.zip', state: 'completed' },
    { id: 'j3', url: 'http://example.org/c.zip', name: 'c.zip', state: 'downloading' },
    { id: 'j4', url: 'http://example.org/d.zip', name: 'd.zip', state: 'canceled' },
  ]
  assert.equal(findDuplicate('http://example.org/a.zip?x=1', existing), undefined)
  assert.equal(findDuplicate('http://example.org/b.zip', existing)?.id, 'j2')
  assert.equal(findDuplicate('http://EXAMPLE.org/c.zip?sig=9', existing)?.id, 'j3')
  assert.equal(findDuplicate('http://example.org/d.zip', existing), undefined)
  assert.equal(duplicateWhere({ state: 'completed' }), 'already downloaded')
  assert.equal(duplicateWhere({ state: 'downloading' }), 'already downloading')
})

test('a duplicate choice maps to zeusd overwrite policy', () => {
  assert.equal(overwriteFor('replace', 'rename'), 'replace')
  assert.equal(overwriteFor('again', 'replace'), 'rename')
  assert.equal(overwriteFor('skip', 'fail'), 'fail')
  assert.equal(overwriteFor(undefined, 'fail'), 'fail')
})

test('an online answer is explained in words', () => {
  assert.match(checkNote({ check: 'offline', status: 403 }), /expired/)
  assert.match(checkNote({ check: 'offline', status: 410 }), /removed/)
  assert.match(checkNote({ check: 'offline', status: 404 }), /no such file/)
  assert.match(checkNote({ check: 'offline', status: 503 }), /retries on its own/)
  assert.match(checkNote({ check: 'needs_login', status: 401 }), /extension/)
  assert.match(checkNote({ check: 'offline', status: 0, error_message: 'refused' }), /refused/)
  const note = checkNote({ check: 'online', status: 206, name: 'My Report.pdf', name_source: 'content_disposition', resumable: 'unsupported', redirects: 3 })
  assert.match(note, /My Report\.pdf/)
  assert.match(note, /no byte ranges/)
  assert.match(note, /3 redirects/)
  assert.equal(checkNote({ check: 'online', status: 206, name: 'a.zip', name_source: 'url_path', resumable: 'verified' }), 'resumable')
})

test('folders must be absolute and inside a download folder', () => {
  const roots = ['C:\\Users\\a\\Downloads\\Zeus']
  assert.equal(folderProblem('C:\\Users\\a\\Downloads\\Zeus\\Video', roots, true), null)
  assert.equal(folderProblem('c:\\users\\a\\downloads\\zeus', roots, true), null)
  assert.match(folderProblem('C:\\Users\\a\\Documents', roots, true) ?? '', /outside/)
  assert.match(folderProblem('Video', roots, true) ?? '', /full folder path/)
  assert.match(folderProblem('C:\\Users\\a\\Downloads\\Zeus\\..\\x', roots, true) ?? '', /\.\./)
  assert.match(folderProblem('', roots, true) ?? '', /Choose/)
  assert.equal(folderProblem('/home/a/dl/x', ['/home/a/dl'], false), null)
  assert.ok(!insideRoot('/home/a/dl', '/home/a/dlx', false))
})

test('the folder for a category follows the rules in order', () => {
  const join = (a, b) => `${a}\\${b}`
  const base = { root: 'D:\\dl', perCategory: true, rememberLast: false, writeRoots: ['D:\\dl', 'E:\\more'] }
  assert.equal(folderFor('video', base, join, true), 'D:\\dl\\Video')
  assert.equal(folderFor('general', base, join, true), 'D:\\dl')
  assert.equal(folderFor('video', { ...base, perCategory: false }, join, true), 'D:\\dl')
  assert.equal(folderFor('video', { ...base, categoryDirs: { video: 'E:\\more\\Films' } }, join, true), 'E:\\more\\Films')
  // A rule pointing outside the download folders is ignored, not followed.
  assert.equal(folderFor('video', { ...base, categoryDirs: { video: 'C:\\elsewhere' } }, join, true), 'D:\\dl\\Video')
  assert.equal(folderFor('music', { ...base, rememberLast: true, lastDirs: { music: 'D:\\dl\\Tunes' } }, join, true), 'D:\\dl\\Tunes')
  assert.equal(folderFor('music', { ...base, rememberLast: false, lastDirs: { music: 'D:\\dl\\Tunes' } }, join, true), 'D:\\dl\\Music')
  assert.equal(folderFor('music', { ...base, categoryDirs: { music: 'E:\\more\\M' }, rememberLast: true, lastDirs: { music: 'D:\\dl\\Tunes' } }, join, true), 'E:\\more\\M')
})

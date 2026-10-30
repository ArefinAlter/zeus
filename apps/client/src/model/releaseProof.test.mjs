import assert from 'node:assert/strict'
import { test } from 'node:test'
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { assertReleaseVersion, assertReusableSidecars, powershellLiteral, selectBundleAssets } from '../../scripts/release-proof.mjs'

test('a release cannot relabel binaries using a different asset version', () => {
  assert.doesNotThrow(() => assertReleaseVersion('0.1.0-alpha.2', '0.1.0-alpha.2'))
  assert.throws(() => assertReleaseVersion('0.1.0-alpha.2', '0.1.0-alpha.1'))
  assert.throws(() => assertReleaseVersion("0.1.0'; exit", "0.1.0'; exit"))
})
test('same-version stale sources and modified cached binaries are rejected', () => {
  const expected = { version: '0.1.0-alpha.2', sources: { zeus: 'a'.repeat(40), core: 'b'.repeat(40) }, binaries: { zeus: 'c'.repeat(64) } }
  assert.doesNotThrow(() => assertReusableSidecars(structuredClone(expected), expected))
  const old = structuredClone(expected); old.sources.core = 'd'.repeat(40)
  assert.throws(() => assertReusableSidecars(old, expected))
  const changed = structuredClone(expected); changed.binaries.zeus = 'e'.repeat(64)
  assert.throws(() => assertReusableSidecars(changed, expected))
  assert.throws(() => assertReusableSidecars(null, expected))
})
test('PowerShell archive paths retain literal quotes and substitution characters', () => {
  assert.equal(powershellLiteral("D:\\It's $(literal)\\release.zip"), "'D:\\It''s $(literal)\\release.zip'")
})

test('release inventory refuses missing or ambiguous installers and excludes unrequested cached bundles', () => {
  const root = mkdtempSync(join(tmpdir(), 'zeus-release-proof-'))
  const version = '0.1.0-alpha.2'
  try {
    for (const kind of ['nsis', 'deb', 'appimage']) mkdirSync(join(root, kind))
    writeFileSync(join(root, 'deb', `Zeus_${version}_amd64.deb`), 'current deb')
    writeFileSync(join(root, 'appimage', `Zeus_${version}_amd64.AppImage`), 'unrequested cached image')
    writeFileSync(join(root, 'nsis', 'Zeus_0.1.0-alpha.1_x64-setup.exe'), 'old installer')
    assert.deepEqual(selectBundleAssets(root, version, 'deb', 'linux'), [join(root, 'deb', `Zeus_${version}_amd64.deb`)])
    assert.throws(() => selectBundleAssets(root, version, 'nsis', 'win32'))
    assert.throws(() => selectBundleAssets(root, version, 'deb,deb', 'linux'))
    assert.throws(() => selectBundleAssets(root, version, 'nsis', 'linux'))
    writeFileSync(join(root, 'deb', `other_${version}_amd64.deb`), 'ambiguous installer')
    assert.throws(() => selectBundleAssets(root, version, 'deb', 'linux'))
  } finally { rmSync(root, { recursive: true, force: true }) }
})

import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync, readdirSync } from 'node:fs'
import { join } from 'node:path'

export function assertReleaseVersion(version, configured) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(configured) || version !== configured) {
    throw new Error('Release asset version must exactly match tauri.conf.json; update source versions before building.')
  }
}

export function gitSource(name, directory) {
  const git = (...args) => {
    const result = spawnSync(process.env.ZEUS_GIT ?? 'git', ['-C', directory, ...args], { encoding: 'utf8', maxBuffer: 1024 * 1024, windowsHide: true })
    if (result.status !== 0) throw new Error(`Cannot verify the ${name} source checkout.`)
    return result.stdout.trim()
  }
  if (git('status', '--porcelain', '--untracked-files=normal')) throw new Error(`The ${name} release source is not clean. Commit the reviewed changes before building.`)
  const commit = git('rev-parse', 'HEAD')
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error(`Invalid ${name} source revision.`)
  return commit
}

export function assertReusableSidecars(record, expected) {
  try { assert.deepEqual(record, expected) } catch {
    throw new Error('Cached sidecars do not match the current source commits, version and binary hashes. Build without --skip-daemon.')
  }
}

export function powershellLiteral(value) { return `'${value.replaceAll("'", "''")}'` }

export function supportedBundles(bundles, platform) {
  const kinds = bundles.split(',')
  const allowed = platform === 'win32' ? ['nsis'] : platform === 'linux' ? ['deb', 'appimage'] : []
  if (!kinds.length || new Set(kinds).size !== kinds.length || kinds.some(kind => !allowed.includes(kind))) {
    throw new Error('Select only supported bundles for this platform.')
  }
  return kinds
}

export function selectBundleAssets(directory, version, bundles, platform) {
  const kinds = supportedBundles(bundles, platform)
  const suffixes = { nsis: /setup\.exe$/, deb: /\.deb$/, appimage: /\.AppImage$/ }
  return kinds.map(kind => {
    const folder = join(directory, kind)
    const matches = existsSync(folder) ? readdirSync(folder).filter(name => name.includes(`_${version}_`) && suffixes[kind].test(name)) : []
    if (matches.length !== 1) throw new Error(`Expected exactly one ${version} ${kind} installer; found ${matches.length}.`)
    return join(folder, matches[0])
  })
}

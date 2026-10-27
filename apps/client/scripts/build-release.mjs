// Builds every Windows or Linux release asset of the Zeus desktop app from this checkout:
//
//   node scripts/build-release.mjs --out <dir> [--version 0.0.1b] [--skip-daemon] [--skip-extension] [--bundles nsis|deb,appimage]
//
//  1. the browser extension package (scripts/package-extension.mjs; needs the zeus-extension checkout beside this one)
//  2. zeusd, zeus, zeus-native-host and zeus-web-bridge in release mode (the daemon workspace, then the bridge)
//  3. the sidecars copied to src-tauri/binaries/<name>-<target triple>[.exe] for Tauri's externalBin
//  4. the interface (npm run build) and the app with its installers (tauri build, merged with tauri.bundle.conf.json)
//  5. the assets in <dir>: installer(s), the portable archive (Windows), the CLI/daemon archive, the extension zips,
//     SHA256SUMS.txt
//
// CARGO_TARGET_DIR, CARGO_BUILD_JOBS and npm_config_cache are respected (keep them on a big disk). Nothing is
// uploaded, committed or pushed here.

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const client = resolve(here, '..')
const repo = resolve(client, '..', '..')
const shell = join(client, 'src-tauri')
const win = process.platform === 'win32'
const arg = (n, d) => {
  const i = process.argv.indexOf(n)
  return i > 0 ? process.argv[i + 1] : d
}
const flag = (n) => process.argv.includes(n)
const out = resolve(arg('--out', join(client, 'dist', 'release')))
const version = arg('--version', '0.0.1b')
const bundles = arg('--bundles', win ? 'nsis' : 'deb,appimage')
const triple = win ? 'x86_64-pc-windows-msvc' : 'x86_64-unknown-linux-gnu'
const exe = win ? '.exe' : ''
const OS = win ? 'windows-x64' : 'linux-x64'
const target = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : null

function run(cmd, args, opts = {}) {
  console.log(`\n> ${cmd} ${args.join(' ')}`)
  const r = spawnSync(cmd, args, { stdio: 'inherit', shell: win && /^(npm|npx)/.test(cmd), ...opts })
  if (r.status !== 0) {
    console.error(`${cmd} ${args.join(' ')} failed (${r.status ?? r.signal})`)
    process.exit(r.status ?? 1)
  }
}

const sha = (p) => createHash('sha256').update(readFileSync(p)).digest('hex')
mkdirSync(out, { recursive: true })

if (!flag('--skip-extension')) run('node', [join(here, 'package-extension.mjs')], { cwd: client })

const release = (name) => join(target ?? join(repo, 'target'), 'release', `${name}${exe}`)
const bridgeTarget = target ? join(target, 'bridge') : join(repo, 'services', 'pre-alpha', 'gateway', 'target')
if (!flag('--skip-daemon')) {
  run('cargo', ['build', '--release', '--locked', '-p', 'zeus-daemon', '-p', 'zeus-cli', '-p', 'zeus-native-host'], { cwd: repo })
  run('cargo', ['build', '--release', '--locked'], { cwd: join(repo, 'services', 'pre-alpha', 'gateway'), env: { ...process.env, ...(target ? { CARGO_TARGET_DIR: bridgeTarget } : {}) } })
}

// Sidecars for the installer.
const bin = join(shell, 'binaries')
mkdirSync(bin, { recursive: true })
for (const name of ['zeusd', 'zeus', 'zeus-native-host']) copyFileSync(release(name), join(bin, `${name}-${triple}${exe}`))

run('npm', ['run', 'build'], { cwd: client })
run('npx', ['tauri', 'build', '--config', 'src-tauri/tauri.bundle.conf.json', '--bundles', bundles], { cwd: client })

// The installer's default folder. Tauri's per-user default is %LOCALAPPDATA%\Zeus, which is also where zeusd keeps its
// own state when it runs on its own (spec 03 section 3.1), so a person with the command-line Zeus would find the app
// mixed into that folder. Put the app under %LOCALAPPDATA%\Programs\Zeus instead: the generated NSIS script is patched
// and compiled again with the toolset Tauri downloaded.
if (win && bundles.split(',').includes('nsis')) {
  const releaseDir = join(target ?? shell + '/target', 'release')
  const nsisDir = join(releaseDir, 'nsis', 'x64')
  const script = join(nsisDir, 'installer.nsi')
  const toolset = join(process.env.LOCALAPPDATA ?? '', 'tauri', 'NSIS')
  const makensis = join(toolset, 'Bin', 'makensis.exe')
  const setup = readdirSync(join(releaseDir, 'bundle', 'nsis')).find((f) => /setup\.exe$/.test(f))
  if (!existsSync(script) || !existsSync(makensis) || !setup) {
    console.error('cannot adjust the installer folder: the generated NSIS script or toolset is missing')
    process.exit(1)
  }
  const from = 'StrCpy $INSTDIR "$LOCALAPPDATA\\${PRODUCTNAME}"'
  const text = readFileSync(script, 'utf8')
  if (!text.includes(from)) {
    console.error('cannot adjust the installer folder: the NSIS script has changed shape')
    process.exit(1)
  }
  writeFileSync(script, text.replace(from, 'StrCpy $INSTDIR "$LOCALAPPDATA\\Programs\\${PRODUCTNAME}"'))
  rmSync(join(nsisDir, 'nsis-output.exe'), { force: true })
  run(makensis, ['-INPUTCHARSET', 'UTF8', '-OUTPUTCHARSET', 'UTF8', '-V2', 'installer.nsi'], { cwd: nsisDir, env: { ...process.env, NSISDIR: toolset, NSISCONFDIR: toolset } })
  copyFileSync(join(nsisDir, 'nsis-output.exe'), join(releaseDir, 'bundle', 'nsis', setup))
}

const appExe = release('zeus-client')
const assets = []
const add = (p) => assets.push(p)

// Installers.
const bundleDir = join(target ?? shell + '/target', 'release', 'bundle')
const pick = (dir, re) => (existsSync(dir) ? readdirSync(dir).filter((f) => re.test(f)).map((f) => join(dir, f)) : [])
for (const f of [...pick(join(bundleDir, 'nsis'), /setup\.exe$/), ...pick(join(bundleDir, 'deb'), /\.deb$/), ...pick(join(bundleDir, 'appimage'), /\.AppImage$/)]) {
  const dest = join(out, f.split(/[\\/]/).pop())
  copyFileSync(f, dest)
  add(dest)
}

// The portable archive (Windows): the app, its sidecars and an empty data folder that selects portable mode.
if (win) {
  const dir = join(out, 'portable', 'Zeus')
  rmSync(join(out, 'portable'), { recursive: true, force: true })
  mkdirSync(join(dir, 'ZeusData'), { recursive: true })
  copyFileSync(appExe, join(dir, 'zeus-client.exe'))
  for (const name of ['zeusd', 'zeus', 'zeus-native-host']) copyFileSync(release(name), join(dir, `${name}.exe`))
  copyFileSync(join(repo, 'LICENSE'), join(dir, 'LICENSE'))
  copyFileSync(join(repo, 'NOTICE'), join(dir, 'NOTICE'))
  writeFileSync(join(dir, 'README.txt'), 'Zeus 0.0.1b, portable. Run zeus-client.exe. Everything the app keeps is in the ZeusData folder next to it; delete the folder to remove it.\nUnsigned: Windows SmartScreen warns before the first run (More info, Run anyway). Check SHA256SUMS.txt.\n')
  const zip = join(out, `zeus-client-${version}-${OS}-portable.zip`)
  rmSync(zip, { force: true })
  run('powershell', ['-NoProfile', '-Command', `Compress-Archive -Path '${dir}' -DestinationPath '${zip}' -CompressionLevel Optimal`])
  rmSync(join(out, 'portable'), { recursive: true, force: true })
  add(zip)
}

// The command-line and daemon archive.
{
  const name = `zeus-cli-${version}-${OS}`
  const dir = join(out, name)
  rmSync(dir, { recursive: true, force: true })
  mkdirSync(dir, { recursive: true })
  for (const n of ['zeus', 'zeusd', 'zeus-native-host']) copyFileSync(release(n), join(dir, `${n}${exe}`))
  const bridge = join(bridgeTarget, 'release', `zeus-web-bridge${exe}`)
  if (existsSync(bridge)) copyFileSync(bridge, join(dir, `zeus-web-bridge${exe}`))
  copyFileSync(join(repo, 'LICENSE'), join(dir, 'LICENSE'))
  copyFileSync(join(repo, 'NOTICE'), join(dir, 'NOTICE'))
  if (win) {
    const zip = join(out, `${name}.zip`)
    rmSync(zip, { force: true })
    run('powershell', ['-NoProfile', '-Command', `Compress-Archive -Path '${dir}' -DestinationPath '${zip}' -CompressionLevel Optimal`])
    add(zip)
  } else {
    const tgz = join(out, `${name}.tar.gz`)
    run('tar', ['-czf', tgz, '-C', out, name])
    add(tgz)
  }
  rmSync(dir, { recursive: true, force: true })
}

// The browser extension packages.
for (const z of ['zeus-extension-chromium.zip', 'zeus-extension-firefox.zip']) {
  const from = join(client, 'dist', 'extension', z)
  if (existsSync(from)) {
    const dest = join(out, z.replace('.zip', `-${version}.zip`))
    copyFileSync(from, dest)
    add(dest)
  }
}

const lines = assets.map((p) => `${sha(p)}  ${p.split(/[\\/]/).pop()}`)
writeFileSync(join(out, 'SHA256SUMS.txt'), `${lines.join('\n')}\n`)
console.log(`\nassets in ${out}:`)
for (const p of assets) console.log(`  ${(statSync(p).size / 1048576).toFixed(1).padStart(7)} MiB  ${p.split(/[\\/]/).pop()}`)
console.log(lines.join('\n'))

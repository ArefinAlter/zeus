// Builds the Zeus browser extension (A:\zeus-extension, MV3) and packages it for the desktop app:
//
//   node scripts/package-extension.mjs [--repo PATH] [--skip-build]
//
// Writes, under apps/client/dist/extension/ (git-ignored):
//   zeus-extension-chromium.zip   the unpacked Chrome, Edge and Brave build (dist/chrome)
//   zeus-extension-firefox.zip    the Firefox build (dist/firefox); release Firefox loads it only as a temporary add-on
//   chromium/, firefox/           the same, unpacked
//   extension.json                version, the development extension ids, sizes and SHA-256 of the zips
// and copies the zips and extension.json to apps/client/src-tauri/resources/extension/ (git-ignored), which the app's
// build embeds (crates/zeus-app-integration/build.rs) so the installer carries them and the app can unpack them.
//
// The extension is built in its own checkout; nothing there is changed except its git-ignored dist/ folder. The dev
// build carries a fixed public key, so every unpacked copy has the same id (docs/EXTENSION_ID.md in that repo). The
// store build (no key, id assigned by the store) is not packaged here: its id is not known before publication.

import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { deflateRawSync } from 'node:zlib'

const here = dirname(fileURLToPath(import.meta.url))
const client = resolve(here, '..')
const arg = (name) => {
  const i = process.argv.indexOf(name)
  return i > 0 ? process.argv[i + 1] : undefined
}
const repo = resolve(arg('--repo') ?? process.env.ZEUS_EXTENSION_REPO ?? join(client, '..', '..', '..', 'zeus-extension'))
const out = join(client, 'dist', 'extension')
const resources = join(client, 'src-tauri', 'resources', 'extension')

if (!existsSync(join(repo, 'package.json'))) {
  console.error(`the extension checkout was not found at ${repo} (set --repo or ZEUS_EXTENSION_REPO)`)
  process.exit(1)
}

if (!process.argv.includes('--skip-build')) {
  const win = process.platform === 'win32'
  const r = spawnSync(win ? 'npm.cmd' : 'npm', ['run', 'build'], {
    cwd: repo,
    stdio: 'inherit',
    shell: win,
    env: { ...process.env, npm_config_cache: process.env.npm_config_cache ?? 'D:\\verne-build\\zeus\\node-cache' },
  })
  if (r.status !== 0) {
    console.error('the extension build failed')
    process.exit(r.status ?? 1)
  }
}

// ---------------------------------------------------------------- ids

/** The Chromium extension id of a manifest `key`: 16 bytes of SHA-256 of the DER public key, as letters a to p. */
function chromiumId(key) {
  const digest = createHash('sha256').update(Buffer.from(key, 'base64')).digest().subarray(0, 16)
  return [...digest].map((b) => String.fromCharCode(97 + (b >> 4)) + String.fromCharCode(97 + (b & 15))).join('')
}

const chromeDir = join(repo, 'dist', 'chrome')
const firefoxDir = join(repo, 'dist', 'firefox')
for (const d of [chromeDir, firefoxDir]) {
  if (!existsSync(join(d, 'manifest.json'))) {
    console.error(`${d} has no manifest.json; build the extension first`)
    process.exit(1)
  }
}
const chromeManifest = JSON.parse(readFileSync(join(chromeDir, 'manifest.json'), 'utf8'))
const firefoxManifest = JSON.parse(readFileSync(join(firefoxDir, 'manifest.json'), 'utf8'))
if (typeof chromeManifest.key !== 'string') {
  console.error('dist/chrome has no manifest key: this is not the development build, so its id is not fixed')
  process.exit(1)
}
const devId = chromiumId(chromeManifest.key)
const firefoxId = firefoxManifest.browser_specific_settings?.gecko?.id
const documented = /Dev extension id: `([a-p]{32})`/.exec(readFileSync(join(repo, 'docs', 'EXTENSION_ID.md'), 'utf8'))?.[1]
if (documented && documented !== devId) {
  console.error(`the derived id ${devId} differs from docs/EXTENSION_ID.md (${documented})`)
  process.exit(1)
}

// ---------------------------------------------------------------- a small zip writer (store a folder, deflate)

const CRC = (() => {
  const t = new Uint32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    t[n] = c >>> 0
  }
  return t
})()
const crc32 = (buf) => {
  let c = 0xffffffff
  for (const b of buf) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

function walk(dir) {
  const files = []
  for (const name of readdirSync(dir).sort()) {
    const p = join(dir, name)
    if (statSync(p).isDirectory()) files.push(...walk(p))
    else files.push(p)
  }
  return files
}

/** Zip `dir` with the files at the archive root, fixed timestamps (a reproducible archive). */
function zipDir(dir) {
  const parts = []
  const central = []
  let offset = 0
  const dosTime = 0
  const dosDate = (2026 - 1980) * 512 + 1 * 32 + 1
  for (const file of walk(dir)) {
    const name = Buffer.from(relative(dir, file).split(sep).join('/'))
    const data = readFileSync(file)
    const packed = deflateRawSync(data, { level: 9 })
    const method = packed.length < data.length ? 8 : 0
    const body = method === 8 ? packed : data
    const crc = crc32(data)
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(0x0800, 6)
    local.writeUInt16LE(method, 8)
    local.writeUInt16LE(dosTime, 10)
    local.writeUInt16LE(dosDate, 12)
    local.writeUInt32LE(crc, 14)
    local.writeUInt32LE(body.length, 18)
    local.writeUInt32LE(data.length, 22)
    local.writeUInt16LE(name.length, 26)
    parts.push(local, name, body)
    const entry = Buffer.alloc(46)
    entry.writeUInt32LE(0x02014b50, 0)
    entry.writeUInt16LE(20, 4)
    entry.writeUInt16LE(20, 6)
    entry.writeUInt16LE(0x0800, 8)
    entry.writeUInt16LE(method, 10)
    entry.writeUInt16LE(dosTime, 12)
    entry.writeUInt16LE(dosDate, 14)
    entry.writeUInt32LE(crc, 16)
    entry.writeUInt32LE(body.length, 20)
    entry.writeUInt32LE(data.length, 24)
    entry.writeUInt16LE(name.length, 28)
    entry.writeUInt32LE(offset, 42)
    central.push(entry, name)
    offset += local.length + name.length + body.length
  }
  const centralBytes = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  const count = central.length / 2
  end.writeUInt16LE(count, 8)
  end.writeUInt16LE(count, 10)
  end.writeUInt32LE(centralBytes.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...parts, centralBytes, end])
}

// ---------------------------------------------------------------- write

rmSync(out, { recursive: true, force: true })
mkdirSync(out, { recursive: true })
cpSync(chromeDir, join(out, 'chromium'), { recursive: true })
cpSync(firefoxDir, join(out, 'firefox'), { recursive: true })
const sha = (b) => createHash('sha256').update(b).digest('hex')
const files = {}
for (const [name, dir] of [['zeus-extension-chromium.zip', chromeDir], ['zeus-extension-firefox.zip', firefoxDir]]) {
  const zip = zipDir(dir)
  writeFileSync(join(out, name), zip)
  files[name] = { bytes: zip.length, sha256: sha(zip) }
}
const meta = {
  name: 'Zeus browser extension',
  version: chromeManifest.version,
  version_name: chromeManifest.version_name ?? chromeManifest.version,
  chromium_dev_id: devId,
  firefox_id: firefoxId ?? null,
  store_ids: { chrome_web_store: null, edge_addons: null, firefox_amo: null },
  note: 'Development build: its Chromium id comes from a fixed public key. Store ids are assigned on first upload and are not known yet.',
  files,
}
writeFileSync(join(out, 'extension.json'), JSON.stringify(meta, null, 2) + '\n')

rmSync(resources, { recursive: true, force: true })
mkdirSync(resources, { recursive: true })
for (const name of [...Object.keys(files), 'extension.json']) cpSync(join(out, name), join(resources, name))

console.log(`packaged ${meta.name} ${meta.version_name}`)
console.log(`  chromium dev id  ${devId}`)
console.log(`  firefox id       ${firefoxId}`)
for (const [name, f] of Object.entries(files)) console.log(`  ${name}  ${f.bytes} bytes  sha256 ${f.sha256}`)
console.log(`  output           ${out}`)
console.log(`  embedded copy    ${resources}`)

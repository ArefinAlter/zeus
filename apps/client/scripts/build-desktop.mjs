// Builds the desktop app (Tauri 2) around the web build, on the scripted backend, and copies the executable to
// apps/client/dist/desktop/ (git-ignored):
//
//   node scripts/build-desktop.mjs
//
// The shell is its own Cargo workspace, so this never touches the daemon workspace at the repository root. Windows
// needs the WebView2 runtime (Windows 11 ships it) and the MSVC build tools. CARGO_TARGET_DIR is respected; by default
// the build goes to src-tauri/target. Nothing is committed or pushed here.

import { spawnSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const web = resolve(here, '..')
const shell = join(web, 'src-tauri')
const win = process.platform === 'win32'

function run(cmd, args, opts) {
  const r = spawnSync(cmd, args, { stdio: 'inherit', ...opts })
  if (r.status !== 0) {
    console.error(`${cmd} ${args.join(' ')} failed (${r.status ?? r.signal})`)
    process.exit(r.status ?? 1)
  }
}

run(win ? 'npm.cmd' : 'npm', ['run', 'build'], { cwd: web, shell: win })
run('cargo', ['build', '--release', '--locked'], { cwd: shell })

const target = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(shell, 'target')
const exe = join(target, 'release', win ? 'zeus-client.exe' : 'zeus-client')
if (!existsSync(exe)) {
  console.error(`expected ${exe}`)
  process.exit(1)
}
const out = join(web, 'dist', 'desktop')
mkdirSync(out, { recursive: true })
const dest = join(out, win ? 'zeus-client.exe' : 'zeus-client')
copyFileSync(exe, dest)
console.log(`\nbuilt ${dest} (${(statSync(dest).size / 1024 / 1024).toFixed(1)} MiB)`)

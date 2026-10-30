// The Go TUI uses the same authenticated local engine/profile as the real desktop.
// All downloads and controls here are confined to generated loopback fixtures and a fresh profile.
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { startFixtures, sha256 } from '../fixture-server.mjs'
import { startHarness, BIN } from '../harness.mjs'
import { join } from 'node:path'

const home = process.env.ZEUS_TUI_EVIDENCE ?? join(tmpdir(), `zeus-tui-${Date.now()}`)
if (existsSync(home)) throw new Error('Use a new scratch profile')
const fx = await startFixtures(0)
let h
try {
  h = await startHarness({ home, fresh: true })
  await h.ready()
  const quickSize = 65536, slowSize = 64 * 1024 * 1024
  const child = spawn(process.env.ZEUS_GO ?? 'go', ['test', './internal/client', '-run', '^TestDesktopIntegration$', '-count=1', '-v'], {
    cwd: process.env.ZEUS_TUI_DIR ?? fileURLToPath(new URL('../../../../../zeus-tui/', import.meta.url)),
    env: {
      ...process.env,
      ZEUS_TUI_INTEGRATION_HOME: h.home,
      ZEUS_TUI_CLI: join(BIN, process.platform === 'win32' ? 'zeus.exe' : 'zeus'),
      ZEUS_TUI_QUICK_URL: `${fx.url}/files/report.pdf?size=${quickSize}&seed=37`,
      ZEUS_TUI_SLOW_URL: `${fx.url}/files/tui-resume.bin?size=${slowSize}&seed=38&rate=1024`,
      ZEUS_TUI_QUICK_SHA: sha256(37, quickSize),
      ZEUS_TUI_SLOW_SHA: sha256(38, slowSize),
    },
    stdio: 'inherit', windowsHide: true,
  })
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) })
  if (code !== 0) throw new Error(`TUI integration failed (${code})`)
  console.log('SCENARIO 14: PASSED')
} finally {
  await h?.stop()
  await fx.close()
}

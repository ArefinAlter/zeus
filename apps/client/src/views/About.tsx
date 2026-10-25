// About: what this release is, plainly. It runs on sample data with no daemon behind it; the interface and the rules
// are real, the data and the agent's replies are prepared.

import { resetAll } from '../backend/store'
import { desktopOs } from '../lib/shell'
import { PREFS_PERSIST } from '../lib/prefs'
import { Chip, Kv, PageHead } from '../ui/components'
import { Mark, VerneMark } from '../ui/icons'

const REAL: [string, string][] = [
  ['Job states and controls', 'The twelve states of spec 01 and the pause, resume, retry and cancel rules, as zeusd applies them.'],
  ['Link review', 'The two-step Preview downloads, Start reviewed downloads flow of the desktop client, with its 50-link and 64 KiB limits.'],
  ['Approvals', 'Facts in a fixed order, the scopes a kind allows, the request hash and bind code, the 750 ms arming delay, typed codes, and refusal when the facts changed.'],
  ['Agent tools', 'The thirteen MCP tools of zeus-mcp with their access levels; no tool decides an approval, at any level.'],
  ['Search syntax and schedules', 'The file search syntax and the queue and limiter rules, covered by tests in this app.'],
]

const PREPARED: [string, string][] = [
  ['Downloads and files', 'Sample jobs on reserved example domains; transfer speeds, failures and retries follow a script. Nothing is downloaded.'],
  ['File index', 'Sample names, sizes and dates in memory. zeusd has no file index yet.'],
  ['Queues and scheduler', 'Run inside this app. zeusd does not schedule queues yet.'],
  ['Agent replies', 'Prepared answers that call the same tools against the sample data. No model is called.'],
  ['Connectors', 'A catalogue with statuses from Zeus\'s implementation ledger. No account is contacted.'],
  ['Request hashes', 'SHA-256 over the canonical facts, standing in for zeusd\'s BLAKE3 over JCS.'],
]

export function About() {
  const os = desktopOs()
  return (
    <div class="page">
      <PageHead title="About Zeus" />
      <div class="panel">
        <div class="about-brand">
          <Mark />
          <div>
            <div class="about-name">Zeus</div>
            <div class="muted">
              <Chip tone="accent">Pre-alpha release</Chip> version {__APP_VERSION__}
            </div>
          </div>
        </div>
        <p>
          Zeus is a local download and file manager that AI assistants can use, and that asks you before it uses a login. This release shows the client: downloads, the link grabber, queues and the speed limiter, file search, agent threads, approvals, connectors and settings.
        </p>
        <p>
          <strong>It runs on sample data with no backend.</strong> It does not connect to zeusd, makes no network request, and downloads nothing. Reloading starts it again.
        </p>
        <Kv
          rows={[
            ['shell', os ? `desktop app (${os}), no daemon` : 'web page, no daemon'],
            ['preferences', PREFS_PERSIST ? 'kept on this device' : 'kept for this visit only; this page uses no browser storage'],
            ['provider keys', 'held in memory for this session only; never stored or sent'],
            ['licence', 'Apache-2.0'],
            ['fonts', 'your system fonts; nothing is fetched'],
          ]}
        />
      </div>
      <h2>What is real</h2>
      <div class="rows card-rows">
        {REAL.map(([k, v]) => (
          <div class="row">
            <div class="text">
              <div class="title">{k}</div>
              <div class="desc">{v}</div>
            </div>
          </div>
        ))}
      </div>
      <h2>What is prepared</h2>
      <div class="rows card-rows">
        {PREPARED.map(([k, v]) => (
          <div class="row">
            <div class="text">
              <div class="title">{k}</div>
              <div class="desc">{v}</div>
            </div>
          </div>
        ))}
      </div>
      <div class="toolbar">
        <button type="button" class="btn" onClick={() => resetAll()}>
          Start the sample data again
        </button>
      </div>
      <div class="about-credit">
        <span>Designed and built by</span>
        <VerneMark />
        <span class="name">Verne Ltd.</span>
      </div>
      <p class="muted small">Copyright 2026 Verne Ltd. · vernesystems.com</p>
    </div>
  )
}

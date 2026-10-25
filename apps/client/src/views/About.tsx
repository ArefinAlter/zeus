// About: what this release is, plainly. It runs on sample data with no daemon behind it; the interface and the rules
// are real, the data and the agent's replies are prepared.

import { HOSTED, LIVE } from '../backend/live'
import { backend } from '../backend/remote'
import { resetAll, useStore } from '../backend/store'
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

const LIVE_REAL: [string, string][] = [
  ['Downloads', 'A real zeusd transfers every file: probing, resume, retries, verification, SHA-256, BLAKE3 and quarantine of executables.'],
  ['Link review', 'Preview downloads is a dry run in zeusd; Start reviewed downloads enqueues exactly that batch once, with a retry key.'],
  ['Controls', 'Pause, resume, retry and cancel go through zeusd\'s own transition checks.'],
  ['Files and integrity', 'The download folder and the integrity facts are zeusd\'s file and artifact records.'],
  ['Approvals', 'zeusd\'s approvals are listed, never decided here: zeusd accepts decisions only from a person on its trust surfaces.'],
  ['The credential', 'zeus-web-bridge holds zeusd\'s restricted gui credential: seven permissions, no approval decisions, no file writes.'],
]

const SERVICE: [string, string][] = [
  ['Queues, scheduler and speed limiter', 'zeusd does not schedule queues or limit speed yet. Queued downloads wait here and go to zeusd when their queue starts; the limiter\'s settings are stored, not applied.'],
  ['Settings', 'Every section is stored for your sandbox. Provider keys are removed before anything is saved and never leave this window.'],
  ['Link grabber and site grabber', 'Online checks and site walks read the sample hosts only. A site plan waits for your approval, which the service holds and checks.'],
  ['Search index', 'A scan of your sandbox folder\'s real files.'],
  ['Agent threads, skills and connectors', 'Stored for your sandbox. The agent\'s replies are prepared; no model is called and no account is contacted.'],
]

export function About() {
  const os = desktopOs()
  const live = useStore((s) => s.live)
  if (LIVE) return <AboutLive os={os} expires={live?.session?.expires_ms ?? null} hosts={live?.session?.fixture_hosts ?? []} version={live?.daemon?.daemon_version ?? null} />
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

function Rows({ rows }: { rows: [string, string][] }) {
  return (
    <div class="rows card-rows">
      {rows.map(([k, v]) => (
        <div class="row">
          <div class="text">
            <div class="title">{k}</div>
            <div class="desc">{v}</div>
          </div>
        </div>
      ))}
    </div>
  )
}

function AboutLive({ os, expires, hosts, version }: { os: ReturnType<typeof desktopOs>; expires: number | null; hosts: string[]; version: string | null }) {
  const b = backend()
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
          Zeus is a local download and file manager that AI assistants can use, and that asks you before it uses a login. This window is the Zeus client: downloads, the link grabber, queues and the speed limiter, file search, agent threads, approvals, connectors and settings.
        </p>
        {HOSTED ? (
          <p>
            <strong>It runs on a real zeusd on the Zeus pre-alpha server, in a private sandbox.</strong> Downloads come only from sample file hosts inside that server ({hosts.join(', ') || 'files.example.org'}); nothing is fetched from the internet. Your sandbox, its files and its settings are deleted after 24 hours.
          </p>
        ) : (
          <p>
            <strong>It runs on your own zeusd</strong>, through zeus-web-bridge at {b.base}. Downloads, files and controls are your daemon's. Queues, settings and lists are kept on this computer.
          </p>
        )}
        <Kv
          rows={[
            ['shell', os ? `desktop app (${os})` : 'web page'],
            ['backend', `${HOSTED ? 'hosted pre-alpha' : 'your zeusd'}${version ? `, zeusd ${version}` : ''}`],
            ...(HOSTED && expires ? ([['sandbox', `ends ${new Date(expires).toLocaleString()}`]] as [string, string][]) : []),
            ['preferences', HOSTED ? 'kept by the pre-alpha service for this sandbox' : 'kept on this device'],
            ['provider keys', 'held in memory for this window only; never stored or sent'],
            ['licence', 'Apache-2.0'],
          ]}
        />
      </div>
      <h2>Served by zeusd</h2>
      <Rows rows={LIVE_REAL} />
      {HOSTED ? (
        <>
          <h2>Served by the pre-alpha service</h2>
          <p class="muted">What zeusd does not implement yet is kept by a small service beside it, for your sandbox only. It is not part of Zeus and goes away as zeusd gains these features.</p>
          <Rows rows={SERVICE} />
        </>
      ) : null}
      <h2>Prepared</h2>
      <Rows
        rows={[
          ['Sample files', HOSTED ? 'The files on the sample hosts are generated: readable documents, random bytes with the extension of their type, one real tarball. Some hosts answer slowly, fail, expire or refuse ranges on purpose.' : 'None: your zeusd downloads what you ask for.'],
          ['Agent replies', 'Prepared answers that call the same tools against this backend. No model is called.'],
          ['Connectors', "A catalogue with statuses from Zeus's implementation ledger. No account is contacted."],
        ]}
      />
      <div class="toolbar">
        <button type="button" class="btn" onClick={() => resetAll()}>
          {HOSTED ? "Clear this sandbox's settings and lists" : 'Clear the settings kept on this device'}
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

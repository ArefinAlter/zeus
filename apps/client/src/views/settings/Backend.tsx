// Backend: which Zeus this window talks to, and what each part of the screen comes from.
//
// The desktop uses its local engine. Optional alternatives are a local authenticated bridge or offline sample data.

import { useState } from 'preact/hooks'

import { HOSTED, LIVE, NATIVE } from '../../backend/live'
import { IN_APP, LOCAL_URL, SWITCHABLE, backend, call, hasLocalToken, setLocalToken, switchBackend } from '../../backend/remote'
import type { BackendKind } from '../../backend/remote'
import { useStore } from '../../backend/store'
import { fmtBytes } from '../../model/units.ts'
import { Chip, CodeCopy, Kv, Notice, PageHead, SettingRow, SettingsGroup } from '../../ui/components'
import { EngineSettings } from './Engine'

const NATIVE_DETAILS: Record<string, string> = {
  'Queues, scheduler, speed limiter': 'The local engine holds, schedules and limits downloads per queue.',
  'Settings, categories, file types': 'Settings stay in this desktop profile; supported provider credentials use the OS credential store.',
  'Link-grabber lists and online checks': 'URL checks and saved lists in this app.',
  'Site grabber walks and plans': 'Site scans and download plans shown for review.',
  'Search index': 'An index of folders you have granted Zeus permission to read.',
  'Agent threads, skills, connectors': 'Configured model providers or installed CLI agents, subject to their sign-in, access and usage limits.',
}

const SOURCES: [string, string, string][] = [
  ['Downloads, states and controls', 'zeusd', 'jobs.list, jobs.enqueue (dry run, then the reviewed batch) and jobs.control through zeus-web-bridge'],
  ['Files in the download folder', 'zeusd', 'files.list: sizes, SHA-256, quarantine, the job that wrote each file'],
  ['Integrity facts', 'zeusd', 'artifacts.list: SHA-256 and BLAKE3 of completed files'],
  ['Approvals listing', 'zeusd', 'approvals.list, read only; decisions stay on zeusd\'s trust surfaces'],
  ['Workflow reports', 'zeusd', 'workflows.list'],
  ['Queues, scheduler, speed limiter', 'pre-alpha service', 'zeusd does not schedule queues or limit speed yet; queued downloads go to zeusd when their queue starts'],
  ['Settings, categories, file types', 'pre-alpha service', 'every section is stored per sandbox; provider keys never leave this window'],
  ['Link-grabber lists and online checks', 'pre-alpha service', 'HEAD requests to the sample hosts only'],
  ['Site grabber walks and plans', 'pre-alpha service', 'reads the sample site; the plan waits for your approval, held by the service'],
  ['Search index', 'pre-alpha service', 'a scan of your sandbox folder\'s real files'],
  ['Agent threads, skills, connectors', 'pre-alpha service', 'stored per sandbox; the agent\'s replies are prepared, no model is called'],
]

export function BackendSettings() {
  const live = useStore((s) => s.live)
  const cur = backend()
  const [kind, setKind] = useState<BackendKind>(cur.kind)
  const [localUrl, setLocalUrl] = useState(cur.kind === 'local' ? cur.base : LOCAL_URL)
  const [token, setToken] = useState('')
  const [tokenSet, setTokenSet] = useState(hasLocalToken())
  const [terminalError, setTerminalError] = useState('')
  const sess = live?.session
  const d = live?.daemon
  const urlOk = /^http:\/\/(127\.0\.0\.1|localhost|\[::1\]):\d{2,5}$/.test(localUrl)

  return (
    <>
      <PageHead title="Backend" sub="Which Zeus this window talks to." />
      <SettingsGroup title="This window">
        <SettingRow title="Backend" desc={cur.kind === 'native' ? 'The Zeus engine on this computer: a bundled zeusd that this app set up, runs and talks to with its own restricted credential.' : cur.kind === 'hosted' ? `The hosted pre-alpha at ${cur.base || window.location.origin}: a real zeusd behind zeus-web-bridge, in a private sandbox.` : cur.kind === 'local' ? `Your own zeusd through zeus-web-bridge at ${cur.base}.` : 'Sample data inside the app. No network request is made.'}>
          <Chip tone={cur.kind === 'scripted' ? 'warn' : live?.connected ? 'ok' : 'bad'}>{cur.kind === 'scripted' ? 'sample data' : live?.connected ? 'connected' : 'not connected'}</Chip>
        </SettingRow>
        {live?.failure ? <Notice kind="error">{live.failure.message}</Notice> : null}
        {LIVE ? (
          <div class="row stack">
            <Kv
              rows={[
                ['zeusd', d ? `${d.daemon_version}${d.journal_ok ? ', journal healthy' : ', journal check failed'}` : '…'],
                [NATIVE ? 'app' : 'bridge', sess ? (NATIVE ? `Zeus ${sess.bridge_version}, ${sess.platform ?? ''}` : `zeus-web-bridge ${sess.bridge_version} (${sess.mode})`) : '…'],
                ['credential', d ? `${NATIVE ? 'app' : 'gui'}, ${d.permissions.length} permissions: ${d.permissions.join(', ')}; it can decide no approval` : '…'],
                ['capabilities', d ? d.capabilities.join(', ') : '…'],
                ...(HOSTED && sess
                  ? ([
                      ['sandbox', `private to this browser; ends ${sess.expires_ms ? new Date(sess.expires_ms).toLocaleString() : 'in 24 hours'}`],
                      ['quota', sess.quota ? `${sess.quota.files} files, ${fmtBytes(sess.quota.bytes)} per sandbox; files are deleted after 24 hours` : 'none'],
                      ['download hosts', sess.fixture_hosts.join(', ')],
                    ] as [string, string][])
                  : []),
              ]}
            />
          </div>
        ) : null}
      </SettingsGroup>

      {NATIVE ? <EngineSettings /> : null}

      {cur.kind === 'local' ? (
        <SettingsGroup title="Bridge token" note="zeus-web-bridge prints the path of its owner-only token file when it starts. The token is held in this window's memory only; paste it again after a restart.">
          <div class="row stack">
            <div class="inline">
              <input class="mono" type="password" aria-label="Bridge token" placeholder={tokenSet ? 'token set for this window' : 'paste the contents of bridge.token'} value={token} onInput={(e) => setToken((e.target as HTMLInputElement).value)} autocomplete="off" />
              <button
                type="button"
                class="btn small primary"
                disabled={!/^[0-9a-f]{32}$/.test(token.trim())}
                onClick={() => {
                  setLocalToken(token)
                  setToken('')
                  setTokenSet(true)
                }}
              >
                Use token
              </button>
            </div>
          </div>
        </SettingsGroup>
      ) : null}

      {SWITCHABLE ? (
        <SettingsGroup title="Switch backend" note="The window reloads. Settings you changed are kept with the backend they were made on.">
          <div class="row stack">
            {IN_APP ? (
              <label class="radio">
                <input type="radio" name="backend" checked={kind === 'native'} onChange={() => setKind('native')} /> This computer: the Zeus engine inside this app (default)
              </label>
            ) : null}
            <label class="radio">
              <input type="radio" name="backend" checked={kind === 'local'} onChange={() => setKind('local')} /> My own zeusd, through zeus-web-bridge on this computer
            </label>
            {kind === 'local' ? <input class="mono" aria-label="Bridge address" value={localUrl} onInput={(e) => setLocalUrl((e.target as HTMLInputElement).value.trim())} /> : null}
            <label class="radio">
              <input type="radio" name="backend" checked={kind === 'scripted'} onChange={() => setKind('scripted')} /> Sample data, offline
            </label>
            <div class="toolbar">
              <button type="button" class="btn small primary" disabled={(kind === cur.kind && (kind !== 'local' || localUrl === cur.base)) || (kind === 'local' && !urlOk)} onClick={() => switchBackend({ kind, base: kind === 'local' ? localUrl : '' })}>
                Switch and reload
              </button>
            </div>
          </div>
        </SettingsGroup>
      ) : null}

      {NATIVE ? <SettingsGroup title="Terminal companion">
        <SettingRow title="Zeus TUI" desc="Open a terminal with this desktop's download queue, progress, pause/resume controls and downloaded files. Keep the desktop running.">
          <button type="button" class="btn small" onClick={async () => {
            setTerminalError('')
            try { await call('POST', '/native/v1/cloud/review-terminal', { action: 'download-tui' }) }
            catch (error) { setTerminalError(error instanceof Error ? error.message : 'The terminal could not be opened.') }
          }}>Open TUI</button>
        </SettingRow>
        {terminalError ? <Notice kind="error">{terminalError}</Notice> : null}
      </SettingsGroup> : null}

      {NATIVE ? null : <SettingsGroup title="Your own zeusd" note="zeusd has one listener, a local IPC socket. zeus-web-bridge is the separate HTTP bridge for this client: it uses zeusd's restricted gui credential (seven permissions, no approval decisions, no file writes), listens on 127.0.0.1 only and requires its token on every request.">
        <div class="row stack">
          <CodeCopy text={'zeus daemon install\nzeusd run\nzeus-web-bridge        # prints its address and the path of bridge.token'} />
        </div>
      </SettingsGroup>}

      {LIVE ? (
        <SettingsGroup title="Where each part comes from">
          {SOURCES.filter(([, from]) => HOSTED || NATIVE || from === 'zeusd').map(([what, from, how]) => (
            <SettingRow title={what} desc={NATIVE ? (NATIVE_DETAILS[what] ?? how.replace('through zeus-web-bridge', 'through the app')) : how}>
              <Chip tone={from === 'zeusd' ? 'ok' : ''}>{NATIVE && from !== 'zeusd' ? 'this app' : from}</Chip>
            </SettingRow>
          ))}
          {!HOSTED && !NATIVE ? <SettingRow title="Everything else" desc="Queues, settings, lists and threads are kept on this computer (the window's own storage)."><Chip>this device</Chip></SettingRow> : null}
        </SettingsGroup>
      ) : null}

      <SettingsGroup title="Elsewhere">
        <SettingRow title="Another computer" desc="zeusd has one local listener and no remote control; a phone or another computer would need a separately designed, authenticated transport." planned />
        <SettingRow title="Android and iOS" desc="No mobile app exists. Phones need native background-transfer and storage adapters first." planned />
      </SettingsGroup>
    </>
  )
}

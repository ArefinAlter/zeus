// The Zeus engine inside the desktop app: whether it runs, where it keeps things, restart, and the settings that live in
// zeusd's configuration file (they apply when the engine restarts).

import { useEffect, useState } from 'preact/hooks'

import { applyEngineSettings } from '../../backend/live'
import { native } from '../../backend/remote'
import type { EngineInfo } from '../../backend/remote'
import { toast, useStore } from '../../backend/store'
import { Chip, CodeCopy, Notice, SettingRow, SettingsGroup } from '../../ui/components'
import { Icon } from '../../ui/icons'

const PHASE: Record<EngineInfo['status']['phase'], [string, 'ok' | 'warn' | 'bad' | '']> = {
  starting: ['starting', 'warn'],
  ready: ['running', 'ok'],
  restarting: ['restarting', 'warn'],
  failed: ['not running', 'bad'],
  stopped: ['stopped', 'bad'],
}

export function EngineSettings() {
  const live = useStore((s) => s.live)
  const [info, setInfo] = useState<EngineInfo | null>(null)
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    let alive = true
    const load = () => native.engine().then((e) => alive && setInfo(e), () => undefined)
    void load()
    const t = setInterval(load, 3000)
    return () => {
      alive = false
      clearInterval(t)
    }
  }, [])
  const status = info?.status ?? live?.engine ?? null
  const [label, tone] = status ? PHASE[status.phase] : ['…', '' as const]
  const dirty = !!live?.engineDirty
  const open = (path: string) => native.open(path, 'open').catch((e: unknown) => toast({ kind: 'warn', title: 'Could not open the folder', desc: e instanceof Error ? e.message : '', important: false, topic: 'system' }))
  return (
    <SettingsGroup title="The Zeus engine" note="Zeus on this computer is two parts: this window and zeusd, the engine that downloads, verifies and keeps the records. This app set up its own copy of Zeus in its own folder and runs it for you; closing Zeus stops it.">
      <SettingRow title="Engine" desc={status?.message ?? ''}>
        <Chip tone={tone}>{label}</Chip>
        <button
          type="button"
          class="btn small"
          disabled={busy}
          onClick={() => {
            setBusy(true)
            native.restart().then(() => toast({ kind: '', title: 'Restarting the engine', desc: 'Downloads in progress resume from the bytes already saved.', important: false, topic: 'system' }), () => undefined).finally(() => setBusy(false))
          }}
        >
          <Icon name="refresh" /> Restart
        </button>
      </SettingRow>
      {status?.phase === 'failed' ? <Notice kind="error">{status.message} The engine writes its log to {info?.logs ?? 'the logs folder'}.</Notice> : null}
      {dirty ? (
        <Notice kind="warn">
          Engine settings changed (download folders, connections, timeouts). They are written to zeusd's configuration and take effect when the engine restarts; downloads in progress resume.{' '}
          <button
            type="button"
            class="btn small primary"
            disabled={busy}
            onClick={() => {
              setBusy(true)
              applyEngineSettings().then(
                () => toast({ kind: 'ok', title: 'Engine settings applied', desc: 'zeusd restarted with the new configuration.', important: false, topic: 'system' }),
                (e: unknown) => toast({ kind: 'bad', title: 'Not applied', desc: e instanceof Error ? e.message : 'The settings were refused.', important: true, topic: 'system' }),
              ).finally(() => setBusy(false))
            }}
          >
            Apply and restart the engine
          </button>
        </Notice>
      ) : null}
      {info ? (
        <>
          <SettingRow title="Version" desc={`zeusd ${status?.version ?? '…'}, app ${info.app_version}${status?.adopted ? '; a running engine from the last session was taken over' : ''}${status && status.restarts ? `; restarted ${status.restarts} time${status.restarts === 1 ? '' : 's'} this session` : ''}`} />
          <SettingRow title="Data folder" desc="The journal, credentials, settings and logs of this app. Uninstalling Zeus leaves it in place; delete it to remove your records.">
            <span class="mono small">{info.home}</span>
            <button type="button" class="btn small ghost" onClick={() => void open(info.home)}>
              Open
            </button>
          </SettingRow>
          <SettingRow title="Log" desc="What the engine reported. It contains no passwords, cookies or link query strings.">
            <span class="mono small">{info.logs}</span>
            <button type="button" class="btn small ghost" onClick={() => void open(info.logs)}>
              Open
            </button>
          </SettingRow>
          <div class="row stack">
            <p class="muted small">To use this engine from a terminal, give the Zeus command-line tool these variables (the Connect page has them ready to copy):</p>
            <CodeCopy text={Object.entries(info.env).map(([k, v]) => (v.includes(' ') ? `${k}="${v}"` : `${k}=${v}`)).join('\n')} />
          </div>
        </>
      ) : null}
    </SettingsGroup>
  )
}

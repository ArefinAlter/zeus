// Settings, Search and index in the desktop app: the folders the engine indexes, what it skips, and when it rescans.
// Every change is sent to the engine, which saves it in its app folder (app/index/index.json) and applies it at once.

import { useEffect, useState } from 'preact/hooks'

import { filesApi, indexFromWire, indexToWire } from '../../backend/files'
import type { IndexStatus } from '../../backend/files'
import { rescan, updateSettings, useStore } from '../../backend/store'
import { fmtWhen } from '../../model/units.ts'
import { Chip, Notice, NumberInput, PageHead, Provenance, Select, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'

const EVERY: [string, string][] = [
  ['0', 'Only on request'],
  ['1', 'Every hour'],
  ['6', 'Every 6 hours'],
  ['24', 'Every day'],
  ['168', 'Every week'],
]

export function NativeIndexSettings() {
  const ix = useStore((s) => s.settings.index)
  const state = useStore((s) => s.index)
  const [status, setStatus] = useState<IndexStatus | null>(null)
  const [root, setRoot] = useState('')
  const [excl, setExcl] = useState(ix.exclusions.join('\n'))
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let alive = true
    const load = () => filesApi.status().then((s) => alive && setStatus(s), () => undefined)
    void load()
    const t = setInterval(load, 2000)
    return () => {
      alive = false
      clearInterval(t)
    }
  }, [])
  useEffect(() => setExcl(ix.exclusions.join('\n')), [ix.exclusions.join('\n')])

  const save = (next: typeof ix) => {
    updateSettings('index', next)
    setError(null)
    filesApi.putConfig(indexToWire(next)).then(
      (c) => updateSettings('index', indexFromWire(c, next)),
      (e: unknown) => {
        setError(e instanceof Error ? e.message : 'The engine did not accept that.')
        void filesApi.config().then((c) => updateSettings('index', indexFromWire(c, next)), () => undefined)
      },
    )
  }
  const set = (patch: Partial<typeof ix>) => save({ ...ix, ...patch })
  const per = (path: string) => status?.roots.find((r) => r.path === path)

  return (
    <>
      <PageHead title="Search and index" sub="What the file search covers. Only folders you include are indexed; reading a folder never lets Zeus write to it." />
      {error ? <Notice kind="error">{error}</Notice> : null}
      <SettingsGroup title="Index">
        <SettingRow
          title="State"
          desc={
            state.state === 'scanning'
              ? `${state.verifying ? 'Checking the saved index' : 'Scanning'} ${state.scanning ?? ''}: ${Math.round(state.progress * 100)} %`
              : `Current: ${(status?.entries ?? state.entries ?? 0).toLocaleString('en-US')} entries${status?.capped ? ` (the index stops at ${(status.cap ?? 0).toLocaleString('en-US')}; the rest is not searchable)` : ''}, last full scan ${state.lastScan ? fmtWhen(state.lastScan) : 'not yet'}.`
          }
        >
          <Chip tone={status?.watcher ? 'ok' : 'warn'}>{status?.watcher ? 'following changes' : 'not following changes'}</Chip>
          <button type="button" class="btn small" disabled={state.state === 'scanning'} onClick={() => rescan()}>
            <Icon name="refresh" /> Rescan all
          </button>
        </SettingRow>
        <SettingRow title="Size and date modified" desc="Every entry keeps its size and date; there is nothing to switch." fixed>
          <Chip tone="info">always indexed</Chip>
        </SettingRow>
        <SettingRow title="File contents" desc="Searching inside files is not part of this release. Search matches names, sizes and dates." fixed>
          <Chip tone="warn">not available</Chip>
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Included folders" note="Your download folders are always listed; change them in Settings, Save to. Folders start with search access. Enable Manage files separately to allow your file operations; remote uploads still require a reviewed transfer and human approval. The engine follows changes with the file system watcher where the system offers one, and rescans on the schedule you choose.">
        {ix.roots.map((r, i) => {
          const st = per(r.path)
          return (
            <SettingRow
              title={<span class="mono">{r.path}</span>}
              desc={`${r.download ? 'Download folder' : r.manageFiles ? 'File management enabled' : 'Search only'} · ${(st?.entries ?? 0).toLocaleString('en-US')} entries${st && !st.exists ? ' · the folder is not there right now; its entries stay until it returns' : ''}${st?.last_scan ? ` · scanned ${fmtWhen(st.last_scan)}` : ''}`}
            >
              <Select
                label="Rescan"
                value={String(r.rescanEveryHours ?? 0)}
                onChange={(v) => set({ roots: ix.roots.map((x, j) => (j === i ? { ...x, rescanEveryHours: v === '0' ? null : Number(v) } : x)) })}
                options={EVERY}
              />
              <Switch label={`Follow changes in ${r.path}`} checked={r.monitorChanges} onChange={(v) => set({ roots: ix.roots.map((x, j) => (j === i ? { ...x, monitorChanges: v } : x)) })} />
              {!r.download ? <Switch label={`Manage files in ${r.path}`} checked={!!r.manageFiles} onChange={(v) => set({ roots: ix.roots.map((x, j) => j === i ? { ...x, manageFiles: v } : x) })} /> : null}
              <Switch label={`Include ${r.path}`} checked={r.include} onChange={(v) => set({ roots: ix.roots.map((x, j) => (j === i ? { ...x, include: v } : x)) })} />
              <button type="button" class="btn small ghost" onClick={() => rescan(r.path)}>
                Rescan
              </button>
              {r.download ? null : (
                <button type="button" class="btn small ghost" onClick={() => set({ roots: ix.roots.filter((_, j) => j !== i) })}>
                  Remove
                </button>
              )}
            </SettingRow>
          )
        })}
        <div class="row">
          <div class="text">
            <input class="mono" aria-label="Folder to include" placeholder="Full path of a folder to search" value={root} onInput={(e) => setRoot((e.target as HTMLInputElement).value)} />
          </div>
          <div class="control">
            <button
              type="button"
              class="btn small"
              disabled={!root.trim()}
              onClick={() => {
                set({ roots: [...ix.roots, { path: root.trim(), kind: 'folder', include: true, monitorChanges: true, rescanEveryHours: null }] })
                setRoot('')
              }}
            >
              <Icon name="plus" /> Include folder
            </button>
          </div>
        </div>
      </SettingsGroup>
      <SettingsGroup title="Skip">
        <SettingRow title="Hidden files and folders">
          <Switch label="Exclude hidden files" checked={ix.excludeHidden} onChange={(v) => set({ excludeHidden: v })} />
        </SettingRow>
        <SettingRow title="System files" desc="Windows system files; on other systems this has no effect.">
          <Switch label="Exclude system files" checked={ix.excludeSystem} onChange={(v) => set({ excludeSystem: v })} />
        </SettingRow>
        <SettingRow title="Names and folders to skip" desc="One per line; * and ? allowed. A line with a path separator is matched against the whole path. Zeus's own partial files and trash are always skipped." stack>
          <textarea class="mono" rows={4} aria-label="Exclusions" value={excl} onInput={(e) => setExcl((e.target as HTMLTextAreaElement).value)} onBlur={() => set({ exclusions: excl.split('\n').map((x) => x.trim()).filter(Boolean) })} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="History">
        <SettingRow title="Searches to remember" desc="Kept in this app's folder with your bookmarks.">
          <NumberInput label="Searches to remember" value={ix.maxHistory} min={0} max={500} onChange={(v) => set({ maxHistory: v })} />
        </SettingRow>
      </SettingsGroup>
      <Provenance>The index is the engine's own, saved in this app's data folder (app/index) so the next start needs no scan. It follows changes with the system's file watcher; where a watcher cannot be set up (some network shares) the folder is rescanned on its schedule only.</Provenance>
    </>
  )
}

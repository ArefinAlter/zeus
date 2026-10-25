// The command palette (Ctrl+K): pages, actions, settings sections, themes, downloads, files, pending approvals, agent
// threads and connectors. Keyboard first: arrows move, Enter runs, Escape closes. Ctrl+Alt+A opens it on the themes.

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import { CONNECTORS, STATUS_LABEL } from '../backend/connectors'
import { getState, globalStop, pauseAll, releaseStop, rescan, resumeAll, setLimiter, startQueue, stopQueue } from '../backend/store'
import { KIND_LABEL } from '../model/approvals.ts'
import { DEFAULT_OPTIONS, matches, parseQuery } from '../model/search.ts'
import { fmtBytes } from '../model/units.ts'
import { setPrefs, usePrefs } from '../lib/prefs'
import { href } from '../lib/router'
import { allThemes, swatch } from '../lib/themes'
import { openDialog } from '../lib/ui'
import { NAV } from '../nav'
import { SETTINGS_SECTIONS } from '../views/settings'
import type { IconName } from './icons'
import { Icon } from './icons'

interface Item {
  group: string
  name: string
  meta?: string
  icon: IconName
  balls?: [string, string]
  current?: boolean
  run: () => void
}

export function Palette({ mode, onClose, go }: { mode: 'all' | 'themes'; onClose: () => void; go: (to: string) => void }) {
  const prefs = usePrefs()
  const [q, setQ] = useState('')
  const [sel, setSel] = useState(0)
  const [view, setView] = useState<'all' | 'themes'>(mode)
  const input = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => input.current?.focus(), [view])

  const items = useMemo<Item[]>(() => {
    const s = getState()
    const out: Item[] = []
    const open = (to: string) => () => (go(to), onClose())
    const act = (f: () => void) => () => (onClose(), f())
    const needle = q.trim().toLowerCase()
    if (view === 'themes') {
      for (const t of allThemes(prefs.customThemes)) {
        const l = swatch(t, 'light')
        const d = swatch(t, 'dark')
        out.push({ group: 'Change theme', name: t.label, icon: 'palette', balls: [l.primary, d.primary], current: t.id === prefs.theme, run: () => (setPrefs({ theme: t.id }), onClose()) })
      }
      return needle ? out.filter((i) => i.name.toLowerCase().includes(needle)) : out
    }
    out.push({ group: 'Actions', name: 'Add URL', meta: 'Ctrl+Alt+N', icon: 'plus', run: act(() => openDialog({ kind: 'addUrl' })) })
    out.push({ group: 'Actions', name: 'Grab links from text or a page', icon: 'grab', run: open(href('grabber')) })
    out.push({ group: 'Actions', name: 'Site grabber', icon: 'globe', run: act(() => openDialog({ kind: 'site' })) })
    out.push({ group: 'Actions', name: 'Pause all downloads', icon: 'pause', run: act(() => void pauseAll()) })
    out.push({ group: 'Actions', name: 'Resume all downloads', icon: 'play', run: act(() => void resumeAll()) })
    out.push({ group: 'Actions', name: s.limiter.enabled ? 'Turn the speed limiter off' : 'Turn the speed limiter on', meta: `${fmtBytes(s.limiter.limitKiBps * 1024)}/s`, icon: 'gauge', run: act(() => setLimiter({ enabled: !s.limiter.enabled })) })
    for (const qu of s.queues) out.push({ group: 'Actions', name: `${qu.running ? 'Stop' : 'Start'} ${qu.name}`, icon: qu.running ? 'stop' : 'play', run: act(() => (qu.running ? stopQueue(qu.id) : startQueue(qu.id))) })
    out.push({ group: 'Actions', name: 'Rescan the file index', icon: 'refresh', run: act(() => rescan()) })
    out.push({ group: 'Actions', name: s.killSwitch ? 'Release the global stop' : 'Global stop: pause everything, revoke site permissions', icon: 'stop', run: act(() => (s.killSwitch ? releaseStop() : openDialog({ kind: 'stop' }))) })
    out.push({ group: 'Agent', name: 'New thread', icon: 'compose', run: open(href('agent')) })
    for (const t of s.threads.slice(0, 8)) out.push({ group: 'Agent threads', name: t.title, meta: t.state === 'running' ? 'running' : '', icon: 'chat', run: open(href('agent', t.id)) })
    for (const n of NAV) out.push({ group: 'Pages', name: n.label, icon: n.icon, run: open(href(n.id)) })
    for (const sec of SETTINGS_SECTIONS) out.push({ group: 'Settings', name: sec.label, meta: sec.hint, icon: sec.icon, run: open(href('settings', sec.id)) })
    out.push({ group: 'Appearance', name: 'Change theme', icon: 'palette', run: () => (setView('themes'), setQ('')) })
    for (const a of ['system', 'light', 'dark'] as const) out.push({ group: 'Appearance', name: `Appearance: ${a[0]?.toUpperCase()}${a.slice(1)}`, icon: a === 'dark' ? 'moon' : a === 'light' ? 'sun' : 'contrast', current: prefs.appearance === a, run: () => (setPrefs({ appearance: a }), onClose()) })
    for (const a of s.approvals.filter((x) => x.state === 'pending')) out.push({ group: 'Waiting for you', name: `${KIND_LABEL[a.facts.kind]} · ${a.facts.website.replace(/^https?:\/\//, '')}`, meta: a.bindCode, icon: 'seal', run: open(href('approvals', a.id)) })
    const base = needle ? out.filter((i) => `${i.group} ${i.name} ${i.meta ?? ''}`.toLowerCase().includes(needle)) : out
    if (needle.length >= 2) {
      for (const j of s.jobs.filter((x) => x.name.toLowerCase().includes(needle)).slice(0, 6)) base.push({ group: 'Downloads', name: j.name, meta: j.state, icon: 'download', run: open(href('downloads', { job: j.id })) })
      const parsed = parseQuery(q, DEFAULT_OPTIONS)
      if (parsed.ok) for (const f of s.files.filter((x) => !x.jobId && matches(parsed.query, x)).slice(0, 6)) base.push({ group: 'Files', name: f.name, meta: f.dir.replace(/^.*\\Zeus/, '…'), icon: f.isFolder ? 'folder' : 'file', run: open(href('files', { path: f.dir })) })
      for (const c of CONNECTORS.filter((x) => `${x.name} ${x.group}`.toLowerCase().includes(needle)).slice(0, 5)) base.push({ group: 'Connectors', name: c.name, meta: STATUS_LABEL[c.status], icon: 'plug', run: open(href('connectors', c.id)) })
    }
    return base.slice(0, 80)
  }, [q, view, prefs, go, onClose])

  useEffect(() => setSel(0), [q, view])
  useEffect(() => {
    listRef.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' })
  }, [sel])
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') onClose()
    else if (e.key === 'ArrowDown') {
      e.preventDefault()
      setSel((v) => Math.min(v + 1, items.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setSel((v) => Math.max(v - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      items[sel]?.run()
    } else if (e.key === 'Backspace' && !q && view === 'themes') {
      e.preventDefault()
      setView('all')
    }
  }
  let lastGroup = ''
  return (
    <div
      class="palette-back"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div class="palette" role="dialog" aria-modal="true" aria-label="Search" onKeyDown={onKey}>
        <div class="search">
          {view === 'themes' ? (
            <button type="button" class="icon-btn small" aria-label="Back" onClick={() => setView('all')}>
              <Icon name="arrowleft" />
            </button>
          ) : (
            <Icon name="search" />
          )}
          <input ref={input} value={q} placeholder={view === 'themes' ? 'Search themes…' : 'Search pages, actions, settings, downloads, files…'} aria-label="Search" onInput={(e) => setQ((e.target as HTMLInputElement).value)} />
        </div>
        <div class="list" role="listbox" ref={listRef}>
          {items.length === 0 ? <div class="group">Nothing matches.</div> : null}
          {items.map((it, i) => {
            const head = it.group !== lastGroup ? <div class="group">{it.group}</div> : null
            lastGroup = it.group
            return (
              <>
                {head}
                <button type="button" class="item" role="option" aria-selected={i === sel} onMouseEnter={() => setSel(i)} onClick={it.run}>
                  <Icon name={it.icon} />
                  <span class="name">{it.name}</span>
                  {it.meta ? <span class="meta">{it.meta}</span> : null}
                  {it.current ? <span class="meta">Current</span> : null}
                  {it.balls ? (
                    <span class="balls tiny">
                      <i class="ball" style={{ background: it.balls[0] }} />
                      <i class="ball" style={{ background: it.balls[1] }} />
                    </span>
                  ) : null}
                </button>
              </>
            )
          })}
        </div>
        <div class="foot">
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> Navigate
          </span>
          <span>
            <kbd>Enter</kbd> Select
          </span>
          {view === 'themes' ? (
            <span>
              <kbd>Backspace</kbd> Back
            </span>
          ) : null}
          <span>
            <kbd>Esc</kbd> Close
          </span>
        </div>
      </div>
    </div>
  )
}

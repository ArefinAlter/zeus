// The downloads list: every job from every source (you, the browser extension, the terminal, agents and workflows),
// filtered by category or queue from the sidebar, sortable and resizable, with multi-select, a row menu and a
// properties panel. Filters change the view, never authority (docs/design/download-workspace.md).

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import { HOSTED, LIVE, NATIVE } from '../../backend/live'
import { native } from '../../backend/remote'
import { joinPath } from '../../lib/paths.ts'
import { cancel, deleteCompleted, getState, isActive, pause, pauseAll, queueName, redownload, refreshAddress, remove, resume, resumeAll, setPriority, setQueue, startQueue, stopQueue, toast, useStore } from '../../backend/store'
import { CATEGORIES, categoryLabel } from '../../model/categories.ts'
import type { Job } from '../../model/types.ts'
import { fmtBytes, fmtRate } from '../../model/units.ts'
import { getPrefs, setPrefs, usePrefs } from '../../lib/prefs'
import { pointerResize } from '../../lib/pointer-resize'
import { href, navigate } from '../../lib/router'
import { openDialog } from '../../lib/ui'
import type { MenuItem } from '../../ui/components'
import { ContextMenu, Dialog, Empty, Provenance } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { COLUMNS } from './columns'
import { MoveRename, Properties } from './Properties'

export const VIEWS: { id: string; label: string }[] = [
  { id: 'all', label: 'All downloads' },
  { id: 'unfinished', label: 'Unfinished' },
  { id: 'finished', label: 'Finished' },
  ...CATEGORIES.filter((c) => c.id !== 'general').map((c) => ({ id: c.id, label: c.label })),
  { id: 'grabber', label: 'Grabber projects' },
]

export function inView(j: Job, view: string, queue: string | null): boolean {
  if (queue) return j.queue === queue
  switch (view) {
    case 'all':
      return true
    case 'unfinished':
      return j.state !== 'completed' && j.state !== 'canceled'
    case 'finished':
      return j.state === 'completed'
    case 'grabber':
      return !!j.grabberProject
    default:
      return j.category === view
  }
}

function canPause(j: Job): boolean {
  return ['queued', 'probing', 'downloading', 'retry_scheduled', 'awaiting_session'].includes(j.state)
}

function canResume(j: Job): boolean {
  return ['paused', 'failed', 'retry_scheduled', 'queued'].includes(j.state)
}

export function Downloads({ view, queue, focus }: { view: string; queue: string | null; focus?: string }) {
  const jobs = useStore((s) => s.jobs)
  const queues = useStore((s) => s.queues)
  const now = useStore((s) => s.now)
  const limiter = useStore((s) => s.limiter)
  const prefs = usePrefs()
  const [sel, setSel] = useState<string[]>(focus ? [focus] : [])
  const [anchor, setAnchor] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null)
  const [colMenu, setColMenu] = useState<{ x: number; y: number } | null>(null)
  const [props, setProps] = useState<string | null>(focus ?? null)
  const [moving, setMoving] = useState<string | null>(null)
  const [confirm, setConfirm] = useState<{ ids: string[] } | null>(null)
  const [widths, setWidths] = useState<Record<string, number> | null>(null)
  const tableRef = useRef<HTMLDivElement>(null)
  const resizeStop = useRef<() => void>(() => {})
  useEffect(() => () => resizeStop.current(), [view, queue])

  useEffect(() => {
    if (focus) {
      setSel([focus])
      setProps(focus)
    }
  }, [focus])

  const visibleCols = prefs.columns.filter((c) => c.visible).map((c) => ({ pref: c, col: COLUMNS.find((x) => x.id === c.id) })).filter((c): c is { pref: (typeof prefs.columns)[number]; col: (typeof COLUMNS)[number] } => !!c.col)

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase()
    const col = COLUMNS.find((c) => c.id === prefs.sortBy) ?? (COLUMNS[0] as (typeof COLUMNS)[number])
    const dir = prefs.sortDir === 'asc' ? 1 : -1
    return jobs
      .filter((j) => inView(j, view, queue) && (!q || j.name.toLowerCase().includes(q) || j.host.includes(q)))
      .sort((a, b) => {
        const x = col.sort(a)
        const y = col.sort(b)
        return (x < y ? -1 : x > y ? 1 : 0) * dir || b.added - a.added
      })
  }, [jobs, view, queue, query, prefs.sortBy, prefs.sortDir])

  const selected = rows.filter((j) => sel.includes(j.id))
  const title = queue ? (queues.find((q) => q.id === queue)?.name ?? 'Queue') : (VIEWS.find((v) => v.id === view)?.label ?? 'Downloads')
  const active = rows.filter((j) => j.state === 'downloading')
  const speed = active.reduce((n, j) => n + j.rate, 0)

  const click = (e: MouseEvent, id: string) => {
    if (e.shiftKey && anchor) {
      const a = rows.findIndex((r) => r.id === anchor)
      const b = rows.findIndex((r) => r.id === id)
      const [lo, hi] = a < b ? [a, b] : [b, a]
      setSel(rows.slice(lo, hi + 1).map((r) => r.id))
    } else if (e.ctrlKey || e.metaKey) {
      setSel((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]))
      setAnchor(id)
    } else {
      setSel([id])
      setAnchor(id)
      if (props) setProps(id)
    }
  }

  const removeAsk = (ids: string[]) => {
    if (!ids.length) return
    if (prefs.confirmRemove) setConfirm({ ids })
    else remove(ids, false)
  }

  const onKey = (e: KeyboardEvent) => {
    if ((e.target as HTMLElement).tagName === 'INPUT') return
    const i = rows.findIndex((r) => r.id === (anchor ?? sel[0]))
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      const k = Math.max(0, Math.min(rows.length - 1, (i < 0 ? -1 : i) + (e.key === 'ArrowDown' ? 1 : -1)))
      const r = rows[k]
      if (!r) return
      if (e.shiftKey && anchor) {
        const a = rows.findIndex((x) => x.id === anchor)
        const [lo, hi] = a < k ? [a, k] : [k, a]
        setSel(rows.slice(lo, hi + 1).map((x) => x.id))
      } else {
        setSel([r.id])
        setAnchor(r.id)
        if (props) setProps(r.id)
      }
      tableRef.current?.querySelector(`[data-id="${r.id}"]`)?.scrollIntoView({ block: 'nearest' })
    } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'a') {
      e.preventDefault()
      setSel(rows.map((r) => r.id))
    } else if (e.key === 'Delete') {
      e.preventDefault()
      removeAsk(sel)
    } else if (e.key === 'Enter' && sel[0]) {
      e.preventDefault()
      setProps(sel[0])
    } else if (e.key === ' ' && selected.length) {
      e.preventDefault()
      if (selected.some(canPause)) pause(selected.filter(canPause).map((j) => j.id))
      else resume(selected.filter(canResume).map((j) => j.id))
    } else if (e.key === 'Escape') {
      setSel([])
      setProps(null)
    }
  }

  const menuItems = (): MenuItem[] => {
    const one = selected.length === 1 ? (selected[0] as Job) : null
    const ids = selected.map((j) => j.id)
    return [
      { label: 'Open', icon: 'external', disabled: !one || one.state !== 'completed' ? 'Only a finished file opens' : one.quarantined ? 'Quarantined: Zeus never opens it' : false, run: () => (NATIVE && one ? void native.open(joinPath(one.dir, one.name), 'open').catch((e: unknown) => toast({ kind: 'warn', title: 'Could not open the file', desc: e instanceof Error ? e.message : '', important: false, topic: 'system' })) : toast({ kind: '', title: 'Opening files is not part of this release', desc: 'It runs on sample data; there is no file to hand to your system.', important: true, topic: 'system' })) },
      { label: 'Open folder', icon: 'folderopen', disabled: !one, run: () => (NATIVE && one ? void native.open(joinPath(one.dir, one.name), 'reveal').catch(() => native.open(one.dir, 'open').catch(() => undefined)) : one && navigate(href('files', { path: one.dir }))) },
      { label: 'Move or rename…', icon: 'rename', disabled: !one || isActive(one), run: () => one && setMoving(one.id) },
      { sep: true, label: '' },
      { label: 'Resume', icon: 'play', hint: 'Space', disabled: !selected.some(canResume), run: () => resume(ids) },
      { label: 'Pause', icon: 'pause', hint: 'Space', disabled: !selected.some(canPause), run: () => pause(ids) },
      { label: 'Download again', icon: 'redo', disabled: !one || one.state !== 'completed', run: () => one && redownload(one.id) },
      { label: 'Refresh address', icon: 'refresh', disabled: !one || !['failed', 'paused', 'retry_scheduled'].includes(one.state), run: () => one && refreshAddress(one.id) },
      { sep: true, label: '' },
      {
        label: 'Add to queue',
        icon: 'queue',
        disabled: !selected.length,
        sub: [...queues.filter((q) => q.kind === 'download').map((q) => ({ label: q.name, icon: 'queue' as const, run: () => setQueue(ids, q.id) })), { label: 'No queue', run: () => setQueue(ids, null) }],
      },
      { label: 'Priority', icon: 'up', disabled: !selected.length, sub: (['high', 'normal', 'low'] as const).map((p) => ({ label: p, run: () => setPriority(ids, p) })) },
      { sep: true, label: '' },
      { label: 'Cancel', icon: 'stop', disabled: !selected.some((j) => !['completed', 'canceled', 'finalizing'].includes(j.state)), run: () => cancel(ids) },
      { label: 'Remove from list…', icon: 'trash', hint: 'Del', danger: true, disabled: !selected.length, run: () => removeAsk(ids) },
      { sep: true, label: '' },
      { label: 'Properties', icon: 'info', hint: 'Enter', disabled: !one, run: () => one && setProps(one.id) },
    ]
  }

  const saveWidth = (id: string, width: number) => setPrefs({ columns: getPrefs().columns.map((column) => column.id === id ? { ...column, width } : column) })
  const startResize = (e: PointerEvent, id: string, base: number) => {
    if (e.button !== 0 || e.isPrimary === false) return
    resizeStop.current()
    const all: Record<string, number> = Object.fromEntries(prefs.columns.map((c) => [c.id, c.width]))
    resizeStop.current = pointerResize(e, e.currentTarget as HTMLElement, {
      axis: 'x', initial: Number.isFinite(base) ? base : 120,
      bounds: () => ({ min: 56, max: 800 }),
      change: (width) => setWidths({ ...all, [id]: width }),
      finish: (width, cancelled) => { if (!cancelled) saveWidth(id, width); setWidths(null) },
    })
  }
  const resizeKey = (event: KeyboardEvent, id: string, width: number) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
    event.preventDefault(); event.stopPropagation()
    const next = event.key === 'Home' ? 56 : event.key === 'End' ? 800 : Math.max(56, Math.min(800, width + (event.key === 'ArrowRight' ? 1 : -1) * (event.shiftKey ? 50 : 10)))
    saveWidth(id, next)
  }

  const sortBy = (id: string) => setPrefs(prefs.sortBy === id ? { sortDir: prefs.sortDir === 'asc' ? 'desc' : 'asc' } : { sortBy: id, sortDir: id === 'name' || id === 'host' || id === 'category' || id === 'queue' ? 'asc' : 'desc' })
  const widthOf = (id: string, w: number) => widths?.[id] ?? w
  const total = visibleCols.reduce((n, c) => n + widthOf(c.pref.id, c.pref.width), 0)
  const queueObj = queue ? queues.find((q) => q.id === queue) : null
  const propsJob = props ? jobs.find((j) => j.id === props) : undefined

  return (
    <div class={`dl-page ${propsJob ? 'with-drawer' : ''}`}>
      <div class="dl-main">
        <div class="dl-head">
          <div class="titles">
            <h1>{title}</h1>
            <p class="sub">
              {rows.length} file{rows.length === 1 ? '' : 's'}
              {active.length ? ` · ${active.length} downloading at ${fmtRate(speed)}` : ''}
              {selected.length > 1 ? ` · ${selected.length} selected, ${fmtBytes(selected.reduce((n, j) => n + (j.size ?? 0), 0))}` : ''}
            </p>
          </div>
          <label class="searchbox dl-search">
            <Icon name="search" />
            <input type="search" placeholder="Filter by name or host" aria-label="Filter downloads" value={query} onInput={(e) => setQuery((e.target as HTMLInputElement).value)} />
          </label>
        </div>
        <div class="dl-toolbar" role="toolbar" aria-label="Download actions">
          <button type="button" class="btn small primary" onClick={() => openDialog({ kind: 'addUrl' })} title="Add a URL (Ctrl+Alt+N)">
            <Icon name="plus" /> Add URL
          </button>
          <span class="tsep" />
          <button type="button" class="btn small ghost" disabled={!selected.some(canResume)} onClick={() => resume(sel)} title="Resume or retry the selection">
            <Icon name="play" /> Resume
          </button>
          <button type="button" class="btn small ghost" disabled={!selected.some(canPause)} onClick={() => pause(sel)} title="Pause the selection; saved bytes stay">
            <Icon name="pause" /> Pause
          </button>
          <button type="button" class="btn small ghost" onClick={() => pauseAll()} title="Pause everything (Ctrl+Alt+P)" aria-label="Pause all">
            <Icon name="pause" /> <span class="lbl-opt">Pause all</span>
          </button>
          <button type="button" class="btn small ghost" onClick={() => resumeAll()} title="Resume everything paused (Ctrl+Alt+R)" aria-label="Resume all">
            <Icon name="play" /> <span class="lbl-opt">Resume all</span>
          </button>
          <span class="tsep" />
          <button type="button" class="btn small ghost" disabled={!sel.length} onClick={() => removeAsk(sel)} title="Remove the selection from the list (Del)">
            <Icon name="trash" /> Remove
          </button>
          <button type="button" class="btn small ghost" onClick={() => deleteCompleted()} title="Remove finished files from the list; the files stay on disk" aria-label="Clear finished">
            <Icon name="check" /> <span class="lbl-opt">Clear finished</span>
          </button>
          <span class="tsep" />
          {queueObj ? (
            queueObj.running ? (
              <button type="button" class="btn small ghost" onClick={() => stopQueue(queueObj.id)}>
                <Icon name="stop" /> Stop queue
              </button>
            ) : (
              <button type="button" class="btn small ghost" onClick={() => startQueue(queueObj.id)}>
                <Icon name="play" /> Start queue
              </button>
            )
          ) : null}
          <a class="btn small ghost" href={href('grabber')} title="Paste many links or a page (Ctrl+Alt+G)">
            <Icon name="grab" /> Link grabber
          </a>
          <button type="button" class="btn small ghost" onClick={() => openDialog({ kind: 'site' })} title="Walk a site and preview what it would fetch (Ctrl+Alt+W)">
            <Icon name="globe" /> Site grabber
          </button>
          <span class="spacer" />
          <a class={`btn small ghost ${limiter.enabled ? 'on' : ''}`} href={href('queues', { tab: 'limiter' })} title="Speed limiter">
            <Icon name="gauge" /> {limiter.enabled ? `Limit ${fmtBytes(limiter.limitKiBps * 1024)}/s` : 'No limit'}
          </a>
          <button type="button" class="icon-btn" aria-label="Columns" title="Columns" onClick={(e) => setColMenu({ x: (e.currentTarget as HTMLElement).getBoundingClientRect().left - 160, y: (e.currentTarget as HTMLElement).getBoundingClientRect().bottom + 4 })}>
            <Icon name="columns" />
          </button>
        </div>
        {view === 'grabber' && !queue ? <GrabberProjects /> : null}
        <div class="dl-table" ref={tableRef} tabIndex={0} onKeyDown={onKey} aria-label={`${title}, ${rows.length} files`} role="grid" aria-multiselectable="true">
          <table style={{ width: `${total}px` }}>
            <colgroup>
              {visibleCols.map((c) => (
                <col data-col={c.pref.id} style={{ width: `${widthOf(c.pref.id, c.pref.width)}px` }} />
              ))}
            </colgroup>
            <thead>
              <tr>
                {visibleCols.map((c) => (
                  <th
                    data-col={c.col.id}
                    class={`${c.col.num ? 'num' : ''} sortable`}
                    aria-sort={prefs.sortBy === c.col.id ? (prefs.sortDir === 'asc' ? 'ascending' : 'descending') : 'none'}
                    onContextMenu={(e) => {
                      e.preventDefault()
                      setColMenu({ x: e.clientX, y: e.clientY })
                    }}
                  >
                    <button type="button" class="thbtn" onClick={() => sortBy(c.col.id)}>
                      {c.col.label}
                      {prefs.sortBy === c.col.id ? <span class={`sortmark ${prefs.sortDir}`} aria-hidden="true">{prefs.sortDir === 'asc' ? '▲' : '▼'}</span> : null}
                    </button>
                    <span class="resizer" role="separator" tabIndex={0} aria-label={`Resize ${c.col.label} column`} aria-orientation="vertical" aria-valuemin={56} aria-valuemax={800} aria-valuenow={widthOf(c.pref.id, c.pref.width)} onPointerDown={(e) => startResize(e, c.pref.id, widthOf(c.pref.id, c.pref.width))} onKeyDown={(e) => resizeKey(e, c.pref.id, widthOf(c.pref.id, c.pref.width))} />
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((j) => (
                <tr
                  data-id={j.id}
                  key={j.id}
                  aria-selected={sel.includes(j.id)}
                  class={`s-row-${j.state}`}
                  onMouseDown={(e) => {
                    if (e.button === 0) click(e, j.id)
                  }}
                  onDblClick={() => setProps(j.id)}
                  onContextMenu={(e) => {
                    e.preventDefault()
                    if (!sel.includes(j.id)) {
                      setSel([j.id])
                      setAnchor(j.id)
                    }
                    setMenu({ x: e.clientX, y: e.clientY })
                  }}
                >
                  {visibleCols.map((c) => (
                    <td data-col={c.col.id} class={c.col.num ? 'num' : ''}>
                      {c.col.cell(j, now)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
          {rows.length === 0 ? (
            <Empty>
              {query ? 'Nothing matches the filter.' : view === 'unfinished' ? 'Nothing unfinished. Every download is done or canceled.' : queue ? 'This queue is empty. Add files to it from a row menu or the link grabber.' : 'No downloads here yet.'}{' '}
              <button type="button" class="btn small" onClick={() => openDialog({ kind: 'addUrl' })}>
                Add URL
              </button>
            </Empty>
          ) : null}
        </div>
        <Provenance>
          {LIVE
            ? `Downloads, their states, sizes, SHA-256 and quarantine come from ${HOSTED ? 'the hosted zeusd' : 'your zeusd'}; pause, resume, retry and cancel go through its own checks. Categories, queues and priorities are kept by ${HOSTED ? 'the pre-alpha service for this sandbox' : 'this device'}.`
            : "The list, its states and the pause, resume, retry and cancel controls follow zeusd's job model; categories, queues and this sample data belong to this release."}
        </Provenance>
      </div>
      {propsJob ? <Properties job={propsJob} onClose={() => setProps(null)} onMove={() => setMoving(propsJob.id)} /> : null}
      {menu ? <ContextMenu x={menu.x} y={menu.y} items={menuItems()} onClose={() => setMenu(null)} label="Download actions" /> : null}
      {colMenu ? (
        <ContextMenu
          x={colMenu.x}
          y={colMenu.y}
          label="Columns"
          onClose={() => setColMenu(null)}
          items={[
            ...prefs.columns.map((c) => ({
              label: COLUMNS.find((x) => x.id === c.id)?.label ?? c.id,
              icon: (c.visible ? 'check' : undefined) as 'check' | undefined,
              disabled: c.id === 'name',
              run: () => setPrefs({ columns: prefs.columns.map((x) => (x.id === c.id ? { ...x, visible: !x.visible } : x)) }),
            })),
            { sep: true, label: '' },
            { label: 'Reset columns', icon: 'refresh', run: () => setPrefs({ columns: undefined as never }) },
          ]}
        />
      ) : null}
      {moving ? <MoveRename id={moving} onClose={() => setMoving(null)} /> : null}
      {confirm ? <ConfirmRemove ids={confirm.ids} onClose={() => setConfirm(null)} /> : null}
    </div>
  )
}

function ConfirmRemove({ ids, onClose }: { ids: string[]; onClose: () => void }) {
  const jobs = getState().jobs.filter((j) => ids.includes(j.id))
  const finished = jobs.filter((j) => j.state === 'completed')
  const unfinished = jobs.length - finished.length
  const [deleteFiles, setDeleteFiles] = useState(false)
  return (
    <Dialog title={`Remove ${jobs.length} download${jobs.length === 1 ? '' : 's'}?`} onClose={onClose}>
      <p>
        {finished.length ? `${finished.length} finished file${finished.length === 1 ? '' : 's'} leave the list. ` : ''}
        {unfinished ? `${unfinished} unfinished download${unfinished === 1 ? ' is' : 's are'} canceled and ${unfinished === 1 ? 'its' : 'their'} partial data deleted. ` : ''}
      </p>
      {finished.length ? (
        <label class="check">
          <input type="checkbox" checked={deleteFiles} onChange={(e) => setDeleteFiles((e.target as HTMLInputElement).checked)} />
          Also move the finished files to the trash
        </label>
      ) : null}
      {deleteFiles ? <p class="muted small-text">Zeus moves files to its own trash, never deletes them outright; a quarantined file needs an approval to move.</p> : null}
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Keep
        </button>
        <button
          type="button"
          class="btn danger"
          onClick={() => {
            remove(ids, deleteFiles)
            onClose()
          }}
        >
          Remove {jobs.length}
        </button>
      </div>
    </Dialog>
  )
}

function GrabberProjects() {
  const projects = useStore((s) => s.projects)
  const runs = useStore((s) => s.live?.runs ?? null)
  if (!projects.length && !runs) return null
  return (
    <div class="projects">
      {runs ? (
        <div class="project">
          <Icon name="server" />
          <span class="pname">Workflow reports from zeusd</span>
          <span class="spacer" />
          <span class="muted small">
            {runs.length
              ? runs.map((r) => `${r.run_id.slice(0, 12)}… ${r.status}: ${r.files_completed ?? 0}/${r.files_enqueued ?? 0} files, ${r.pages_fetched ?? 0} pages${r.possibly_incomplete ? ', possibly incomplete' : ''}`).join(' · ')
              : 'No workflow runs in this zeusd. Site-grabber plans here are held by the pre-alpha service and send their files to zeusd as reviewed batches.'}
          </span>
        </div>
      ) : null}
      {projects.map((p) => (
        <div class="project">
          <Icon name="globe" />
          <span class="pname">{p.name}</span>
          <span class="muted mono small">{p.startUrl}</span>
          <span class="spacer" />
          <span class="muted small">
            {p.files} files · {fmtBytes(p.bytes)} · depth {p.request.depth}
          </span>
          <span class={`chip ${p.state === 'finished' ? 'ok' : p.state === 'awaiting_approval' ? 'warn' : p.state === 'denied' ? 'bad' : 'accent'}`}>{p.state === 'running' ? (p.queue ? `approved, in ${queueName(p.queue)}` : 'approved, downloading') : p.state.replace('_', ' ')}</span>
          {p.approvalId && p.state === 'awaiting_approval' ? (
            <a class="btn small" href={href('approvals', p.approvalId)}>
              Review
            </a>
          ) : null}
        </div>
      ))}
    </div>
  )
}

export { categoryLabel }

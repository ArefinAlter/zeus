// Files: instant search over an index of the download folders, with the search syntax, result filters, a folders pane,
// a preview pane, bookmarks and search history of a desktop file search tool. zeusd has no file index yet; the design
// is in docs/design/client-workflows-and-platforms.md ("Scoped file index"). In this release the index is sample
// metadata held in memory, and nothing reads a disk.

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import { HOSTED, LIVE } from '../backend/live'
import { addBookmark, clearHistory, getState, pushHistory, removeBookmark, rescan, toast, useStore } from '../backend/store'
import { extOf } from '../model/categories.ts'
import { DEFAULT_OPTIONS, FILTERS, SYNTAX, filterMatch, matches, parseQuery } from '../model/search.ts'
import type { SearchOptions } from '../model/search.ts'
import type { FileEntry } from '../model/types.ts'
import { fmtBytes, fmtStamp, fmtWhen } from '../model/units.ts'
import { setPrefs, usePrefs } from '../lib/prefs'
import { href, navigate } from '../lib/router'
import { categoryOf } from '../model/categories.ts'
import type { MenuItem } from '../ui/components'
import { Chip, ContextMenu, CopyButton, Dialog, Provenance } from '../ui/components'
import { Icon, categoryIcon } from '../ui/icons'

type SortKey = 'name' | 'path' | 'size' | 'modified'

function fullPath(f: FileEntry): string {
  return `${f.dir}\\${f.name}`
}

interface Folder {
  path: string
  name: string
  depth: number
  count: number
}

function folderTree(files: FileEntry[]): Folder[] {
  const counts = new Map<string, number>()
  for (const f of files) {
    if (f.isFolder) continue
    let d = f.dir
    while (d) {
      counts.set(d, (counts.get(d) ?? 0) + 1)
      const i = d.lastIndexOf('\\')
      if (i <= 2) break
      d = d.slice(0, i)
    }
  }
  const roots = getState().settings.index.roots.filter((r) => r.include).map((r) => r.path)
  return [...counts.entries()]
    .filter(([p]) => roots.some((r) => p === r || p.startsWith(r + '\\')))
    .map(([path, count]) => {
      const root = roots.find((r) => path === r || path.startsWith(r + '\\')) ?? ''
      const depth = path === root ? 0 : path.slice(root.length + 1).split('\\').length
      return { path, name: path === root ? root : (path.split('\\').pop() ?? path), depth, count }
    })
    .sort((a, b) => a.path.localeCompare(b.path))
}

/** A small generated picture for image files, from the file's hash: the preview has no file to read in this release. */
function Thumb({ seed }: { seed: string }) {
  let h = 0
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0
  const hue = h % 360
  return (
    <svg class="thumb" viewBox="0 0 120 80" aria-hidden="true">
      <rect width="120" height="80" fill={`hsl(${hue} 35% 78%)`} />
      <circle cx={20 + (h % 60)} cy={20 + (h % 25)} r="10" fill={`hsl(${(hue + 40) % 360} 60% 88%)`} />
      <path d={`M0 80 L${30 + (h % 30)} ${40 + (h % 20)} L${60 + (h % 20)} 62 L${85 + (h % 15)} ${36 + (h % 18)} L120 80Z`} fill={`hsl(${(hue + 180) % 360} 25% 45%)`} />
    </svg>
  )
}

export function Files({ initialPath }: { initialPath?: string }) {
  const files = useStore((s) => s.files)
  const index = useStore((s) => s.index)
  const bookmarks = useStore((s) => s.bookmarks)
  const history = useStore((s) => s.history)
  const now = useStore((s) => s.now)
  const prefs = usePrefs()
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState('everything')
  const [opts, setOpts] = useState<SearchOptions>(DEFAULT_OPTIONS)
  const [folder, setFolder] = useState<string | null>(initialPath ?? null)
  const [sort, setSort] = useState<{ key: SortKey; dir: 1 | -1 }>({ key: 'name', dir: 1 })
  const [sel, setSel] = useState<number | null>(null)
  const [help, setHelp] = useState(false)
  const [saving, setSaving] = useState(false)
  const [menu, setMenu] = useState<{ x: number; y: number; id: number } | null>(null)
  const [scroll, setScroll] = useState(0)
  const [height, setHeight] = useState(600)
  const input = useRef<HTMLInputElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const rowH = prefs.density === 'compact' ? 24 : 30

  useEffect(() => {
    if (initialPath) setFolder(initialPath)
  }, [initialPath])
  useEffect(() => {
    const on = () => input.current?.focus()
    window.addEventListener('zeus:files-search', on)
    input.current?.focus()
    return () => window.removeEventListener('zeus:files-search', on)
  }, [])
  useEffect(() => {
    const el = list.current
    if (!el) return
    const ro = new ResizeObserver(() => setHeight(el.clientHeight))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  const parsed = useMemo(() => parseQuery(query, opts, Date.now()), [query, opts])
  const t0 = performance.now()
  const results = useMemo(() => {
    if (!parsed.ok) return []
    const q = parsed.query
    const inFolder = (f: FileEntry) => !folder || f.dir === folder || f.dir.startsWith(folder + '\\') || (f.isFolder && fullPath(f).startsWith(folder))
    const out = files.filter((f) => inFolder(f) && filterMatch(filter, f) && matches(q, f))
    const k = sort.key
    out.sort((a, b) => {
      const x = k === 'name' ? a.name.toLowerCase() : k === 'path' ? a.dir.toLowerCase() : k === 'size' ? a.size : a.modified
      const y = k === 'name' ? b.name.toLowerCase() : k === 'path' ? b.dir.toLowerCase() : k === 'size' ? b.size : b.modified
      return (x < y ? -1 : x > y ? 1 : 0) * sort.dir || (a.isFolder === b.isFolder ? 0 : a.isFolder ? -1 : 1)
    })
    return out
  }, [files, parsed, filter, folder, sort])
  const took = performance.now() - t0
  const tree = useMemo(() => folderTree(files), [files])
  const current = results.find((f) => f.id === sel) ?? null
  const totalSize = results.reduce((n, f) => n + f.size, 0)

  // Only the rows in view are drawn.
  const first = Math.max(0, Math.floor(scroll / rowH) - 8)
  const last = Math.min(results.length, Math.ceil((scroll + height) / rowH) + 8)
  const shown = results.slice(first, last)

  const commit = () => pushHistory(query)
  const onKey = (e: KeyboardEvent) => {
    const i = results.findIndex((f) => f.id === sel)
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      const k = Math.max(0, Math.min(results.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))
      const f = results[k]
      if (!f) return
      setSel(f.id)
      const el = list.current
      if (el) {
        if (k * rowH < el.scrollTop) el.scrollTop = k * rowH
        else if ((k + 1) * rowH > el.scrollTop + el.clientHeight - rowH) el.scrollTop = (k + 2) * rowH - el.clientHeight
      }
    } else if (e.key === 'Enter' && current?.isFolder) {
      setFolder(fullPath(current))
    }
  }

  const items = (f: FileEntry): MenuItem[] => [
    { label: 'Open', icon: 'external', disabled: f.quarantined ? 'Quarantined: Zeus never opens it' : f.isFolder ? false : false, run: () => (f.isFolder ? setFolder(fullPath(f)) : toast({ kind: '', title: 'Opening files is not part of this release', desc: 'The index holds sample metadata; there is no file to open.', important: true, topic: 'system' })) },
    { label: 'Open folder', icon: 'folderopen', run: () => setFolder(f.dir) },
    { sep: true, label: '' },
    { label: 'Copy full path', icon: 'copy', run: () => void navigator.clipboard?.writeText(fullPath(f)).catch(() => undefined) },
    { label: 'Copy name', icon: 'copy', run: () => void navigator.clipboard?.writeText(f.name).catch(() => undefined) },
    { label: 'Copy SHA-256', icon: 'copy', disabled: !f.sha256, run: () => void navigator.clipboard?.writeText(f.sha256 ?? '').catch(() => undefined) },
    { sep: true, label: '' },
    { label: 'Show in Downloads', icon: 'download', disabled: !f.jobId ? 'Not downloaded by this Zeus' : false, run: () => navigate(href('downloads', { job: f.jobId })) },
    { label: 'Search in this folder', icon: 'search', run: () => setFolder(f.isFolder ? fullPath(f) : f.dir) },
  ]

  const toggle = (k: keyof SearchOptions, label: string, text: string) => (
    <button type="button" class={`opt ${opts[k] ? 'on' : ''}`} aria-pressed={opts[k]} title={label} aria-label={label} onClick={() => setOpts({ ...opts, [k]: !opts[k] })}>
      {text}
    </button>
  )
  const th = (key: SortKey, label: string, num = false) => (
    <div class={`fth ${num ? 'num' : ''}`} role="columnheader" aria-sort={sort.key === key ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none'}>
      <button type="button" class="thbtn" onClick={() => setSort(sort.key === key ? { key, dir: sort.dir === 1 ? -1 : 1 } : { key, dir: key === 'size' || key === 'modified' ? -1 : 1 })}>
        {label}
        {sort.key === key ? <span class="sortmark">{sort.dir === 1 ? '▲' : '▼'}</span> : null}
      </button>
    </div>
  )

  return (
    <div class={`files-page ${prefs.filesFolders ? 'with-folders' : ''} ${prefs.filesPreview ? 'with-preview' : ''}`}>
      <div class="fsearch">
        <label class="bigsearch">
          <Icon name="search" />
          <input
            ref={input}
            type="search"
            spellcheck={false}
            aria-label="Search files"
            placeholder="Search the download folders: name, ext:pdf, size:>100mb, dm:lastweek…"
            value={query}
            onInput={(e) => setQuery((e.target as HTMLInputElement).value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') commit()
              if (e.key === 'ArrowDown') {
                e.preventDefault()
                list.current?.focus()
                if (results[0]) setSel(results[0].id)
              }
            }}
            onBlur={commit}
          />
          {toggle('matchCase', 'Match case', 'Aa')}
          {toggle('wholeWord', 'Match whole word', 'ab|')}
          {toggle('matchPath', 'Match path', 'C:\\')}
          {toggle('regex', 'Regular expression', '.*')}
        </label>
        <select class="ffilter" aria-label="Result filter" value={filter} onChange={(e) => setFilter((e.target as HTMLSelectElement).value)}>
          {FILTERS.map((f) => (
            <option value={f.id}>{f.label}</option>
          ))}
        </select>
        <button type="button" class="icon-btn boxed" aria-label="Search syntax" title="Search syntax" onClick={() => setHelp(true)}>
          <span class="qmark">?</span>
        </button>
        <button type="button" class="icon-btn boxed" aria-label="Bookmark this search" title="Bookmark this search" disabled={!query.trim() && filter === 'everything'} onClick={() => setSaving(true)}>
          <Icon name="star" />
        </button>
        <button type="button" class={`icon-btn boxed ${prefs.filesFolders ? 'on' : ''}`} aria-pressed={prefs.filesFolders} aria-label="Folders pane" title="Folders pane" onClick={() => setPrefs({ filesFolders: !prefs.filesFolders })}>
          <Icon name="panel" />
        </button>
        <button type="button" class={`icon-btn boxed ${prefs.filesPreview ? 'on' : ''}`} aria-pressed={prefs.filesPreview} aria-label="Preview pane" title="Preview pane" onClick={() => setPrefs({ filesPreview: !prefs.filesPreview })}>
          <Icon name="panelright" />
        </button>
      </div>
      {!parsed.ok ? <div class="fsyntaxerr">{parsed.error}</div> : null}
      {folder ? (
        <div class="fscope">
          <Icon name="folder" /> In <span class="mono">{folder}</span>
          <button type="button" class="btn small ghost" onClick={() => setFolder(null)}>
            <Icon name="x" /> Everywhere
          </button>
        </div>
      ) : null}
      <div class="fpanes">
        {prefs.filesFolders ? (
          <aside class="fside" aria-label="Folders, bookmarks and history">
            <div class="fside-head">Folders</div>
            <ul class="ftree">
              {tree.map((f) => (
                <li>
                  <button type="button" class={folder === f.path ? 'on' : ''} style={{ paddingLeft: `${8 + f.depth * 14}px` }} onClick={() => setFolder(folder === f.path ? null : f.path)} title={f.path}>
                    <Icon name={f.depth === 0 ? 'drive' : folder === f.path ? 'folderopen' : 'folder'} />
                    <span class="name">{f.name}</span>
                    <span class="trail">{f.count}</span>
                  </button>
                </li>
              ))}
            </ul>
            <div class="fside-head">Bookmarks</div>
            <ul class="ftree">
              {bookmarks.map((b) => (
                <li class="bm">
                  <button
                    type="button"
                    onClick={() => {
                      setQuery(b.query)
                      setFilter(b.filter)
                    }}
                    title={`${b.query} · ${b.filter}`}
                  >
                    <Icon name="star" />
                    <span class="name">{b.name}</span>
                  </button>
                  <button type="button" class="icon-btn small rowx" aria-label={`Remove bookmark ${b.name}`} onClick={() => removeBookmark(b.id)}>
                    <Icon name="x" />
                  </button>
                </li>
              ))}
            </ul>
            <div class="fside-head">
              Recent searches
              {history.length ? (
                <button type="button" class="btn small ghost" onClick={clearHistory}>
                  Clear
                </button>
              ) : null}
            </div>
            <ul class="ftree">
              {history.slice(0, 8).map((h) => (
                <li>
                  <button type="button" onClick={() => setQuery(h)}>
                    <Icon name="history" />
                    <span class="name mono">{h}</span>
                  </button>
                </li>
              ))}
            </ul>
          </aside>
        ) : null}
        <div class="fresults">
          <div class="fhead" role="row">
            {th('name', 'Name')}
            {th('path', 'Path')}
            {th('size', 'Size', true)}
            {th('modified', 'Date modified')}
          </div>
          <div class="flist" ref={list} tabIndex={0} role="grid" aria-label={`${results.length} results`} aria-rowcount={results.length} onScroll={(e) => setScroll((e.target as HTMLDivElement).scrollTop)} onKeyDown={onKey}>
            <div style={{ height: `${results.length * rowH}px`, position: 'relative' }}>
              {shown.map((f, i) => (
                <div
                  class={`frow ${f.id === sel ? 'on' : ''}`}
                  role="row"
                  aria-selected={f.id === sel}
                  style={{ top: `${(first + i) * rowH}px`, height: `${rowH}px` }}
                  onMouseDown={() => setSel(f.id)}
                  onDblClick={() => (f.isFolder ? setFolder(fullPath(f)) : undefined)}
                  onContextMenu={(e) => {
                    e.preventDefault()
                    setSel(f.id)
                    setMenu({ x: e.clientX, y: e.clientY, id: f.id })
                  }}
                >
                  <span class="fc name">
                    <Icon name={f.isFolder ? 'folder' : categoryIcon(categoryOf(f.name))} />
                    <span class="nm">{f.name}</span>
                    {f.quarantined ? (
                      <span class="qflag" title="Quarantined">
                        <Icon name="shield" />
                      </span>
                    ) : null}
                  </span>
                  <span class="fc path mono">{f.dir}</span>
                  <span class="fc num">{f.isFolder ? '' : fmtBytes(f.size)}</span>
                  <span class="fc">{fmtWhen(f.modified, now)}</span>
                </div>
              ))}
            </div>
            {results.length === 0 ? <div class="empty">{parsed.ok ? 'No file matches. Check the filter and the folder scope.' : 'Fix the search to see results.'}</div> : null}
          </div>
          <div class="fstatus">
            <span>
              {results.length.toLocaleString('en-US')} object{results.length === 1 ? '' : 's'} · {fmtBytes(totalSize)}
            </span>
            <span class="muted">
              {index.state === 'scanning' ? `Scanning ${index.scanning ?? ''}: ${Math.round(index.progress * 100)} %` : `Index current · generation ${index.generation} · ${files.length.toLocaleString('en-US')} entries`}
            </span>
            <span class="spacer" />
            <span class="muted">searched in {took < 1 ? '<1' : took.toFixed(0)} ms</span>
            <button type="button" class="btn small ghost" disabled={index.state === 'scanning'} onClick={() => rescan()}>
              <Icon name="refresh" /> Rescan
            </button>
            <a class="btn small ghost" href={href('settings', 'index')}>
              Index settings
            </a>
          </div>
        </div>
        {prefs.filesPreview ? (
          <aside class="fpreview" aria-label="Preview">
            {current ? (
              <>
                {!current.isFolder && /^(jpg|jpeg|png|webp|heic|tif|svg)$/.test(extOf(current.name)) ? (
                  <Thumb seed={current.sha256 ?? current.name} />
                ) : (
                  <div class="bigicon">
                    <Icon name={current.isFolder ? 'folder' : categoryIcon(categoryOf(current.name))} />
                  </div>
                )}
                <div class="pname">{current.name}</div>
                <dl class="kvs">
                  <dt>folder</dt>
                  <dd class="mono break small">{current.dir}</dd>
                  {!current.isFolder ? (
                    <>
                      <dt>size</dt>
                      <dd>
                        {fmtBytes(current.size)} <span class="muted small">({current.size.toLocaleString('en-US')} bytes)</span>
                      </dd>
                      <dt>type</dt>
                      <dd>{extOf(current.name) ? `.${extOf(current.name)}` : 'none'}</dd>
                    </>
                  ) : null}
                  <dt>modified</dt>
                  <dd>{fmtStamp(current.modified)}</dd>
                  {current.host ? (
                    <>
                      <dt>from</dt>
                      <dd class="mono small">{current.host}</dd>
                    </>
                  ) : null}
                  {current.sha256 ? (
                    <>
                      <dt>SHA-256</dt>
                      <dd class="mono break small">{current.sha256}</dd>
                    </>
                  ) : null}
                  <dt>quarantine</dt>
                  <dd>{current.quarantined ? <Chip tone="warn">quarantined</Chip> : 'no'}</dd>
                </dl>
                <div class="dactions">
                  <CopyButton text={fullPath(current)} label="Copy path" />
                  {current.jobId ? (
                    <a class="btn small" href={href('downloads', { job: current.jobId })}>
                      <Icon name="download" /> In Downloads
                    </a>
                  ) : null}
                </div>
                <p class="muted small">{LIVE ? 'A preview of the contents needs the file itself; this client indexes names, sizes, dates and the digests zeusd recorded.' : 'A preview of the contents needs the file itself. This release indexes sample names, sizes and dates only.'}</p>
              </>
            ) : (
              <div class="pane-empty">Select a file to see its details.</div>
            )}
          </aside>
        ) : null}
      </div>
      <Provenance>
        {LIVE
          ? `zeusd has no file index yet. ${HOSTED ? 'The pre-alpha service indexes the real files of your sandbox folder' : "This client indexes zeusd's file list of your download folder"}; SHA-256, quarantine and the job behind each file come from zeusd. The search syntax and its rules are tested.`
          : 'zeusd has no file index yet (planned: a scoped metadata index of the download folders). This release searches sample metadata in memory; the syntax and its rules are tested.'}
      </Provenance>
      {menu ? (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          label="File actions"
          onClose={() => setMenu(null)}
          items={(() => {
            const f = files.find((x) => x.id === menu.id)
            return f ? items(f) : []
          })()}
        />
      ) : null}
      {help ? <SyntaxHelp onClose={() => setHelp(false)} onUse={(q) => (setQuery(q), setHelp(false))} /> : null}
      {saving ? <SaveBookmark query={query} filter={filter} onClose={() => setSaving(false)} /> : null}
    </div>
  )
}

function SyntaxHelp({ onClose, onUse }: { onClose: () => void; onUse: (q: string) => void }) {
  return (
    <Dialog title="Search syntax" onClose={onClose} wide>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Type</th>
              <th>Finds</th>
            </tr>
          </thead>
          <tbody>
            {SYNTAX.map(([q, d]) => (
              <tr>
                <td>
                  <button type="button" class="linklike mono" onClick={() => onUse(q.split('  ')[0] ?? q)}>
                    {q}
                  </button>
                </td>
                <td>{d}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <p class="muted small">A space means AND and binds loosest; | means OR. The toggles beside the search box apply to every term: match case (Aa), whole word (ab|), match path (C:\) and regular expression (.*).</p>
    </Dialog>
  )
}

function SaveBookmark({ query, filter, onClose }: { query: string; filter: string; onClose: () => void }) {
  const [name, setName] = useState(query || FILTERS.find((f) => f.id === filter)?.label || '')
  return (
    <Dialog title="Bookmark this search" onClose={onClose}>
      <label class="field">
        Name
        <input value={name} onInput={(e) => setName((e.target as HTMLInputElement).value)} />
      </label>
      <p class="muted small mono">
        {query || '(any name)'} · {FILTERS.find((f) => f.id === filter)?.label}
      </p>
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          class="btn primary"
          onClick={() => {
            addBookmark(name, query, filter)
            onClose()
          }}
        >
          Save bookmark
        </button>
      </div>
    </Dialog>
  )
}

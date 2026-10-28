// Files: instant search over an index of the download folders, with the search syntax, result filters, a folders pane,
// a preview pane, bookmarks and the search history of a desktop file search tool.
//
// In the desktop app the index is real: the engine scans the download folders (and folders you add), follows changes
// with the file system watcher, and executes parsed queries in Rust with bounded, generation-aware pages.
// This view virtualizes those rows, preserves their identities across refresh, previews selected files and offers
// scoped file, archive and upload actions. Live folder browsing also includes entries excluded from the index. In the hosted pre-alpha and the offline sample the index is a sandbox
// scan or sample metadata, as before.

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import { filesApi } from '../backend/files'
import type { Preview, SearchPage } from '../backend/files'
import { HOSTED, LIVE, NATIVE } from '../backend/live'
import { addBookmark, clearHistory, pushHistory, removeBookmark, rescan, toast, useStore } from '../backend/store'
import { extOf } from '../model/categories.ts'
import { DEFAULT_OPTIONS, FILTERS, SYNTAX, filterMatch, highlightRanges, matches, parseQuery } from '../model/search.ts'
import type { Query, SearchOptions } from '../model/search.ts'
import type { FileEntry } from '../model/types.ts'
import { fmtBytes, fmtStamp, fmtWhen } from '../model/units.ts'
import { baseOf, isInside, joinPath, parentOf } from '../lib/paths.ts'
import { setPrefs, usePrefs } from '../lib/prefs'
import { href, navigate } from '../lib/router'
import { categoryOf } from '../model/categories.ts'
import type { MenuItem } from '../ui/components'
import { Chip, ContextMenu, CopyButton, Dialog, Provenance } from '../ui/components'
import { Icon, categoryIcon } from '../ui/icons'
import { DeleteDialog, MoveDialog, RenameDialog, NewFolderDialog, CopyJobs } from './files/FileOps'
import { FileInsights } from './files/FileInsights'
import { FileManager } from './files/FileManager'
import { ArchiveDialog, ArchiveJobs } from './files/Archives'
import '../styles/files.css'

type SortKey = 'name' | 'path' | 'size' | 'modified'

function fullPath(f: FileEntry): string {
  return joinPath(f.dir, f.name)
}

interface Folder {
  path: string
  name: string
  depth: number
  count: number
  open: boolean
  hasChildren: boolean
}

/** The folders pane: every folder of an included root with its file count, collapsed below the first level. */
function folderTree(files: FileEntry[], roots: string[], open: Set<string>): Folder[] {
  const counts = new Map<string, number>(roots.map(root=>[root,0]))
  const rootOf = new Map<string, string>()
  const rootFor = (dir: string): string | undefined => {
    let r = rootOf.get(dir)
    if (r === undefined) {
      r = roots.find((x) => isInside(x, dir)) ?? ''
      rootOf.set(dir, r)
    }
    return r || undefined
  }
  for (const f of files) {
    const r = rootFor(f.dir)
    if (!r) continue
    if (f.isFolder) {
      const p = fullPath(f)
      if (!counts.has(p)) counts.set(p, 0)
      continue
    }
    let d = f.dir
    for (let i = 0; i < 64 && isInside(r, d); i++) {
      counts.set(d, (counts.get(d) ?? 0) + 1)
      if (d.length <= r.length) break
      d = parentOf(d)
    }
  }
  const parents = new Set<string>()
  for (const p of counts.keys()) parents.add(parentOf(p))
  const out: Folder[] = []
  for (const [path, count] of counts) {
    const root = rootFor(path)
    if (!root) continue
    const depth = path.length <= root.length ? 0 : path.slice(root.length).split(/[\\/]/).filter(Boolean).length
    // Shown when every folder between it and its root is open.
    let visible = true
    if (depth > 0) {
      let p = parentOf(path)
      for (let i = 0; i < 64; i++) {
        if (!open.has(p)) {
          visible = false
          break
        }
        if (p.length <= root.length) break
        p = parentOf(p)
      }
    }
    if (!visible) continue
    out.push({ path, name: depth === 0 ? root : baseOf(path), depth, count, open: open.has(path), hasChildren: parents.has(path) })
  }
  return out.sort((a, b) => a.path.localeCompare(b.path))
}

/** A small generated picture for image files in the offline sample, from the file's hash. */
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

/** A name with the parts the search matched marked. */
function Name({ name, q }: { name: string; q: Query }) {
  const ranges = q.length ? highlightRanges(q, name) : []
  if (!ranges.length) return <>{name}</>
  const parts: preact.ComponentChildren[] = []
  let at = 0
  for (const [a, b] of ranges) {
    if (a > at) parts.push(name.slice(at, a))
    parts.push(<mark class="hl">{name.slice(a, b)}</mark>)
    at = b
  }
  if (at < name.length) parts.push(name.slice(at))
  return <>{parts}</>
}

type PreviewState = { path: string; data?: Preview; error?: string } | null

function PreviewBody({ pv, current }: { pv: PreviewState; current: FileEntry }) {
  if (!pv || pv.path !== fullPath(current)) return <div class="muted small">Reading the file…</div>
  if (pv.error) return <p class="muted small">{pv.error}</p>
  const d = pv.data
  if (!d) return null
  switch (d.kind) {
    case 'text':
      return (
        <>
          <pre class="fprev-text" tabIndex={0} aria-label="Start of the file">
            {d.text}
          </pre>
          <p class="muted small">
            {d.truncated ? `The first ${fmtBytes((d.text ?? '').length)} of ${fmtBytes(d.size ?? 0)}. ` : ''}
            {typeof d.lines === 'number' ? `${d.lines.toLocaleString('en-US')} line${d.lines === 1 ? '' : 's'}. ` : ''}
            {d.note ?? ''}
          </p>
        </>
      )
    case 'image':
      return (
        <>
          <img class="fprev-img" src={d.data_url} alt={`Preview of ${d.name ?? current.name}`} />
          <p class="muted small">
            {d.width && d.height ? `${d.width} × ${d.height} pixels. ` : ''}
            {d.mime}
          </p>
        </>
      )
    case 'folder':
      return (
        <p class="muted small">
          {(d.entries ?? 0).toLocaleString('en-US')} item{d.entries === 1 ? '' : 's'} below, {fmtBytes(d.bytes ?? 0)} in files.
        </p>
      )
    case 'refused':
      return <p class="fprev-refused">{d.message ?? 'Zeus does not preview this file.'}</p>
    default:
      return <p class="muted small">{d.message ?? 'No preview for this kind of file.'}</p>
  }
}

export function Files({ initialPath }: { initialPath?: string }) {
  const files = useStore((s) => s.files)
  const index = useStore((s) => s.index)
  const bookmarks = useStore((s) => s.bookmarks)
  const history = useStore((s) => s.history)
  const now = useStore((s) => s.now)
  const indexRoots = useStore((s) => s.settings.index.roots)
  const downloadRoots = useStore((s) => s.settings.saveTo.writeRoots)
  const writeRoots = [...downloadRoots, ...indexRoots.filter(r => r.include && r.manageFiles).map(r=>r.path)]
  const prefs = usePrefs()
  const [archive, setArchive] = useState<{kind:'create'|'extract';path:string;name:string}|null>(null)
  const [insights, setInsights] = useState(false)
  const [manager, setManager] = useState(false)
  const [searchFiles, setSearchFiles] = useState<FileEntry[]>([])
  const [searchPage, setSearchPage] = useState<SearchPage | null>(null)
  const [searchBusy, setSearchBusy] = useState(false)
  const [searchError, setSearchError] = useState('')
  const searchEpoch = useRef(0)
  const nativeIds = useRef(new Map<string, number>())
  const nextNativeId = useRef(-1)
  const searchSignature = useRef('')
  const listedFolder = useRef<string | null>(null)
  const nativeId = (path: string): number => {
    const known = nativeIds.current.get(path)
    if (known !== undefined) return known
    const id = nextNativeId.current--
    if (nativeIds.current.size >= 20_000) {
      const oldest = nativeIds.current.keys().next().value
      if (oldest !== undefined) nativeIds.current.delete(oldest)
    }
    nativeIds.current.set(path, id)
    return id
  }
  const [folderFiles, setFolderFiles] = useState<FileEntry[]>([])
  const [nextPage, setNextPage] = useState<string | null>(null)
  const [folderError, setFolderError] = useState('')
  const [listing, setListing] = useState(false)
  const [revision, setRevision] = useState(0)
  const listingEpoch = useRef(0)
  const [query, setQuery] = useState('')
  const [mkdir, setMkdir] = useState<string | null>(null)
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
  const [openDirs, setOpenDirs] = useState<Set<string>>(() => new Set())
  const [pv, setPv] = useState<PreviewState>(null)
  const [op, setOp] = useState<{ kind: 'rename' | 'move' | 'copy' | 'delete'; id: number } | null>(null)
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

  const loadFolder = async (path: string, after: string, epoch: number) => {
    setListing(true)
    setFolderError('')
    try {
      const page = await filesApi.list(path, after)
      if (listingEpoch.current !== epoch) return
      setFolderFiles(previous => {
        const kept = after ? previous : []
        const rows = page.files.map((row) => ({ id: nativeId(row.path), name: baseOf(row.path), dir: parentOf(row.path), isFolder: row.is_dir, size: row.size, modified: row.mtime, quarantined: row.quarantined }))
        return [...kept, ...rows]
      })
      setNextPage(page.next)
    } catch (e) {
      if (listingEpoch.current === epoch) setFolderError(e instanceof Error ? e.message : 'The folder could not be read.')
    } finally {
      if (listingEpoch.current === epoch) setListing(false)
    }
  }
  useEffect(() => {
    const epoch = ++listingEpoch.current
    if (listedFolder.current !== folder) {
      setFolderFiles([])
      setSel(null)
      listedFolder.current = folder
    }
    setNextPage(null)
    if (NATIVE && folder) void loadFolder(folder, '', epoch)
    return () => { listingEpoch.current++ }
  }, [folder, revision])
  const nativeSearch = NATIVE && !folder
  const visibleFiles = NATIVE ? (folder ? folderFiles : searchFiles) : files
  const parsed = useMemo(() => parseQuery(query, opts, Date.now()), [query, opts])
  const loadSearch = async (offset: number, epoch: number, generation?: number) => {
    if (!parsed.ok) return
    setSearchBusy(true)
    setSearchError('')
    try {
      const page = await filesApi.search(parsed.query, filter, sort.key, sort.dir === -1, offset, generation)
      if (searchEpoch.current !== epoch) return
      setSearchFiles(previous => {
        const kept = offset ? previous : []
        return [...kept, ...page.files.map((row) => ({ id: nativeId(row.path), name: baseOf(row.path), dir: parentOf(row.path), isFolder: row.is_dir, size: row.size, modified: row.mtime, quarantined: row.quarantined }))]
      })
      setSearchPage(page)
    } catch (e) {
      if (searchEpoch.current === epoch) setSearchError(e instanceof Error ? e.message : 'The search could not be completed.')
    } finally {
      if (searchEpoch.current === epoch) setSearchBusy(false)
    }
  }
  useEffect(() => {
    const epoch = ++searchEpoch.current
    if (!nativeSearch || !parsed.ok) return
    const signature = JSON.stringify([query, opts, filter, sort])
    if (searchSignature.current !== signature) {
      setSearchFiles([])
      searchSignature.current = signature
      setScroll(0)
      if (list.current) list.current.scrollTop = 0
    }
    setSearchPage(null)
    const timer = setTimeout(() => void loadSearch(0, epoch), 180)
    return () => { clearTimeout(timer); searchEpoch.current++ }
  }, [nativeSearch, parsed, filter, sort, index.generation, revision])
  const t0 = performance.now()
  const results = useMemo(() => {
    if (!parsed.ok) return []
    if (nativeSearch) return searchFiles
    const q = parsed.query
    const inside = folder && !NATIVE ? (f: FileEntry) => isInside(folder, f.dir) || (f.isFolder && isInside(folder, fullPath(f))) : () => true
    const out = visibleFiles.filter((f) => inside(f) && filterMatch(filter, f) && matches(q, f))
    const k = sort.key
    out.sort((a, b) => {
      const x = k === 'name' ? a.name.toLowerCase() : k === 'path' ? a.dir.toLowerCase() : k === 'size' ? a.size : a.modified
      const y = k === 'name' ? b.name.toLowerCase() : k === 'path' ? b.dir.toLowerCase() : k === 'size' ? b.size : b.modified
      return (x < y ? -1 : x > y ? 1 : 0) * sort.dir || (a.isFolder === b.isFolder ? 0 : a.isFolder ? -1 : 1)
    })
    return out
  }, [visibleFiles, parsed, filter, folder, sort, nativeSearch, searchFiles])
  const took = nativeSearch ? searchPage?.elapsed_ms ?? 0 : performance.now() - t0
  const includedRoots = useMemo(() => [...new Set([...downloadRoots, ...indexRoots.filter((r) => r.include).map((r) => r.path)])], [indexRoots, downloadRoots])
  const tree = useMemo(() => {
    const open = new Set(openDirs)
    for (const r of includedRoots) open.add(r)
    return folderTree([...files, ...folderFiles], includedRoots, open)
  }, [files, folderFiles, includedRoots, openDirs])
  const current = results.find((f) => f.id === sel) ?? null
  const totalSize = nativeSearch ? searchPage?.total_bytes ?? 0 : results.reduce((n, f) => n + f.size, 0)
  const matchCount = nativeSearch ? searchPage?.total ?? 0 : results.length
  const hiliteQuery: Query = parsed.ok ? parsed.query : []

  // Only the rows in view are drawn.
  const first = Math.max(0, Math.floor(scroll / rowH) - 8)
  const last = Math.min(results.length, Math.ceil((scroll + height) / rowH) + 8)
  const shown = results.slice(first, last)

  // The preview of the selected file, from the engine.
  const curPath = current ? fullPath(current) : null
  useEffect(() => {
    if (!NATIVE || !prefs.filesPreview || !curPath) {
      setPv(null)
      return
    }
    let alive = true
    const t = setTimeout(() => {
      filesApi.preview(curPath).then(
        (data) => alive && setPv({ path: curPath, data }),
        (e: unknown) => alive && setPv({ path: curPath, error: e instanceof Error ? e.message : 'No preview.' }),
      )
    }, 120)
    return () => {
      alive = false
      clearTimeout(t)
    }
  }, [curPath, current?.size, current?.modified, prefs.filesPreview])

  const commit = () => pushHistory(query)
  const writable = (f: FileEntry): true | string => {
    if (!NATIVE) return 'Zeus changes files only in the desktop app, inside its download folders.'
    if (!writeRoots.some((r) => isInside(r, f.dir))) return 'Zeus changes files only inside its download folders; enable Manage files for this folder in Settings, Search and index.'
    if (f.quarantined) return 'Quarantined: Zeus does not move, rename or delete it here. `zeus files organize` at a terminal asks you first.'
    return true
  }
  const openFile = (f: FileEntry, reveal = false) => {
    if (f.isFolder && !reveal) return setFolder(fullPath(f))
    if (NATIVE) {
      filesApi.open(fullPath(f), reveal ? 'reveal' : 'open').catch((e: unknown) => toast({ kind: 'warn', title: reveal ? 'Could not show the file' : 'Could not open the file', desc: e instanceof Error ? e.message : '', important: false, topic: 'system' }))
    } else if (reveal) setFolder(f.dir)
    else toast({ kind: '', title: 'Opening files is not part of this release', desc: 'The index holds sample metadata; there is no file to open.', important: true, topic: 'system' })
  }
  const act = (kind: 'rename' | 'move' | 'copy' | 'delete', f: FileEntry | null) => {
    if (!f) return
    const ok = kind === 'copy' && NATIVE && !f.quarantined ? true : writable(f)
    if (ok !== true) return void toast({ kind: 'warn', title: 'Not here', desc: ok, important: false, topic: 'system' })
    setOp({ kind, id: f.id })
  }
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
    } else if (e.key === 'Enter' && current) {
      openFile(current)
    } else if (e.key === 'F2') {
      e.preventDefault()
      act('rename', current)
    } else if (e.key === 'Delete') {
      e.preventDefault()
      act('delete', current)
    }
  }

  const items = (f: FileEntry): MenuItem[] => {
    const w = writable(f)
    const why = w === true ? false : w
    return [
      { label: 'Open', icon: 'external', disabled: f.quarantined ? 'Quarantined: Zeus never opens it' : false, run: () => openFile(f) },
      { label: 'Open folder', icon: 'folderopen', run: () => openFile(f, true) },
      { sep: true, label: '' },
      { label: 'Copy full path', icon: 'copy', run: () => void navigator.clipboard?.writeText(fullPath(f)).catch(() => undefined) },
      { label: 'Copy name', icon: 'copy', run: () => void navigator.clipboard?.writeText(f.name).catch(() => undefined) },
      { label: 'Copy SHA-256', icon: 'copy', disabled: !f.sha256, run: () => void navigator.clipboard?.writeText(f.sha256 ?? '').catch(() => undefined) },
      { sep: true, label: '' },
      { label: 'Rename…', icon: 'rename', hint: 'F2', disabled: why, run: () => act('rename', f) },
      { label: 'Copy to folder…', icon: 'copy', disabled: !NATIVE || f.quarantined, run: () => act('copy', f) },
      { label: 'Upload…', icon: 'upload', disabled: !NATIVE || f.quarantined || f.isFolder, run: () => navigate(href('uploads', {path:fullPath(f), name:f.name})) },
      { label: 'Create ZIP…', icon: 'folder', disabled: !NATIVE || f.quarantined, run: () => setArchive({kind:'create',path:fullPath(f),name:`${f.name}.zip`}) },
      { label: 'Extract ZIP…', icon: 'folderopen', disabled: !NATIVE || f.quarantined || f.isFolder || !f.name.toLowerCase().endsWith('.zip'), run: () => setArchive({kind:'extract',path:fullPath(f),name:f.name.replace(/\.zip$/i,'') || 'extracted'}) },
      { label: 'Move…', icon: 'folder', disabled: why, run: () => act('move', f) },
      { label: 'Delete…', icon: 'trash', hint: 'Del', danger: true, disabled: why, run: () => act('delete', f) },
      { sep: true, label: '' },
      { label: 'Show in Downloads', icon: 'download', disabled: !f.jobId ? 'Not downloaded by this Zeus' : false, run: () => navigate(href('downloads', { job: f.jobId })) },
      { label: 'Search in this folder', icon: 'search', run: () => setFolder(f.isFolder ? fullPath(f) : f.dir) },
    ]
  }

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

  const opFile = op ? visibleFiles.find((x) => x.id === op.id) : undefined
  const moveFolders = useMemo(() => ((op?.kind === 'move' || op?.kind === 'copy') ? files.filter((f) => f.isFolder && writeRoots.some((r) => isInside(r, fullPath(f)))).map(fullPath) : []), [op?.kind, files, writeRoots])

  if (NATIVE && manager) return <FileManager roots={includedRoots} writeRoots={writeRoots} initialPath={folder} onClose={() => { setManager(false); setRevision(n => n + 1) }} />

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
            placeholder="Search files: name, ext:pdf, size:>100mb, dm:lastweek…"
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
      {NATIVE ? <div class="fscope"><span>Manage files in folders you choose.</span><button class="btn small" onClick={()=>setManager(true)}>Two-pane manager</button><button class="btn small" onClick={()=>setInsights(true)}>Storage and history</button><button class="btn small" onClick={()=>navigate(href('settings','index'))}>Choose folders</button><button class="btn small" disabled={!folder || !writeRoots.some(r=>isInside(r,folder))} onClick={()=>folder && setMkdir(folder)}>New folder</button>{folder ? <><button class="btn small" disabled={!indexRoots.some(r=>r.include && isInside(r.path,parentOf(folder))) && !downloadRoots.some(r=>isInside(r,parentOf(folder)))} onClick={()=>setFolder(parentOf(folder))}>Up</button><button class="btn small" disabled={listing} onClick={()=>setRevision(n=>n+1)}>Refresh folder</button></> : null}</div> : null}
      {NATIVE && folder ? <div class="fscope" role="status">{listing ? 'Reading folder…' : `${folderFiles.length.toLocaleString()} items loaded`}{nextPage ? <button class="btn small" disabled={listing} onClick={()=>void loadFolder(folder,nextPage,listingEpoch.current)}>Load more</button> : null}{folderError ? <span class="field-err">{folderError}</span> : null}</div> : null}
      {nativeSearch ? <div class="fscope" role="status">{searchBusy ? 'Searching the scoped index…' : `${results.length.toLocaleString()} of ${matchCount.toLocaleString()} matches loaded`}{searchPage?.next != null ? <button class="btn small" disabled={searchBusy} onClick={()=>void loadSearch(searchPage.next!,searchEpoch.current,searchPage.generation)}>Load more matches</button> : null}{searchError ? <span class="field-err">{searchError} <button class="btn small" onClick={()=>setRevision(n=>n+1)}>Refresh search</button></span> : null}</div> : null}
      {!parsed.ok ? <div class="fsyntaxerr">{parsed.error}</div> : null}
      {folder ? (
        <div class="fscope">
          <Icon name="folder" /> In <span class="mono">{folder}</span>
          <button type="button" class="btn small ghost" onClick={() => setFolder(null)}>
            <Icon name="x" /> Everywhere
          </button>
        </div>
      ) : null}
      {NATIVE && (index.freshness === 'scan_incomplete' || index.snapshotError) ? <div class="fscope" role="status"><span class="field-err">{index.snapshotError ? 'The index snapshot could not be saved; Zeus will retry. ' : ''}{index.freshness === 'scan_incomplete' ? `${index.unavailableRoots ?? 0} unavailable or changed folders; ${index.unreadableDirectories ?? 0} directories could not be fully read. Search may be incomplete.` : ''}</span><button class="btn small" onClick={()=>rescan()}>Reconcile index</button></div> : null}
      {NATIVE && index.state === 'scanning' ? (
        <div class="fprogress" role="status">
          <span class="bar" style={{ width: `${Math.round(index.progress * 100)}%` }} />
          <span class="txt">
            {index.verifying ? 'Checking the saved index' : 'Indexing'} {index.scanning ? `· ${index.scanning}` : ''}: {Math.round(index.progress * 100)} %
          </span>
        </div>
      ) : null}
      <div class="fpanes">
        {prefs.filesFolders ? (
          <aside class="fside" aria-label="Folders, bookmarks and history">
            <div class="fside-head">Folders</div>
            <ul class="ftree">
              {tree.map((f) => (
                <li>
                  <button
                    type="button"
                    class={folder === f.path ? 'on' : ''}
                    style={{ paddingLeft: `${8 + f.depth * 14}px` }}
                    onClick={() => {
                      setFolder(folder === f.path ? null : f.path)
                      if (f.hasChildren) {
                        const next = new Set(openDirs)
                        if (next.has(f.path)) next.delete(f.path)
                        else next.add(f.path)
                        setOpenDirs(next)
                      }
                    }}
                    title={f.path}
                  >
                    <Icon name={f.depth === 0 ? 'drive' : folder === f.path || f.open ? 'folderopen' : 'folder'} />
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
                  data-path={fullPath(f)}
                  style={{ top: `${(first + i) * rowH}px`, height: `${rowH}px` }}
                  onMouseDown={() => setSel(f.id)}
                  onDblClick={() => openFile(f)}
                  onContextMenu={(e) => {
                    e.preventDefault()
                    setSel(f.id)
                    setMenu({ x: e.clientX, y: e.clientY, id: f.id })
                  }}
                >
                  <span class="fc name">
                    <Icon name={f.isFolder ? 'folder' : categoryIcon(categoryOf(f.name))} />
                    <span class="nm">
                      <Name name={f.name} q={hiliteQuery} />
                    </span>
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
            {results.length === 0 ? (
              <div class="empty">
                {!parsed.ok
                  ? 'Fix the search to see results.'
                  : (index.entries ?? files.length) === 0 && NATIVE
                    ? index.state === 'scanning'
                      ? 'Indexing your folders…'
                      : 'Nothing is indexed yet. Downloads appear here as they finish. Add other folders in Search and index settings.'
                    : 'No file matches. Check the filter and the folder scope.'}
              </div>
            ) : null}
          </div>
          <div class="fstatus">
            <span>
              {matchCount.toLocaleString('en-US')} object{matchCount === 1 ? '' : 's'} · {fmtBytes(totalSize)}
            </span>
            <span class="muted">
              {index.state === 'scanning'
                ? `${index.verifying ? 'Checking' : 'Scanning'} ${index.scanning ?? ''}: ${Math.round(index.progress * 100)} %`
                : `${index.freshness === 'scan_incomplete' ? 'Index incomplete' : 'Index current'} · generation ${index.generation} · ${(NATIVE ? index.entries ?? 0 : files.length).toLocaleString('en-US')} entries${NATIVE ? (index.watcher ? ' · following changes' : ' · not following changes') : ''}${index.capped ? ' · capped' : ''}`}
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
                {NATIVE ? null : !current.isFolder && /^(jpg|jpeg|png|webp|heic|tif|svg)$/.test(extOf(current.name)) ? (
                  <Thumb seed={current.sha256 ?? current.name} />
                ) : (
                  <div class="bigicon">
                    <Icon name={current.isFolder ? 'folder' : categoryIcon(categoryOf(current.name))} />
                  </div>
                )}
                <div class="pname">{current.name}</div>
                {NATIVE ? (
                  <div class="fprev">
                    <PreviewBody pv={pv} current={current} />
                  </div>
                ) : null}
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
                  {pv?.data?.blake3 && pv.path === fullPath(current) ? (
                    <>
                      <dt>BLAKE3</dt>
                      <dd class="mono break small">{pv.data.blake3}</dd>
                    </>
                  ) : null}
                  <dt>quarantine</dt>
                  <dd>{current.quarantined ? <Chip tone="warn">quarantined</Chip> : 'no'}</dd>
                </dl>
                <div class="dactions">
                  <CopyButton text={fullPath(current)} label="Copy path" />
                  {NATIVE ? (
                    <button type="button" class="btn small" onClick={() => openFile(current, true)}>
                      <Icon name="folderopen" /> Show in folder
                    </button>
                  ) : null}
                  {current.jobId ? (
                    <a class="btn small" href={href('downloads', { job: current.jobId })}>
                      <Icon name="download" /> In Downloads
                    </a>
                  ) : null}
                </div>
                {NATIVE ? null : <p class="muted small">{LIVE ? 'A preview of the contents needs the file itself; this client indexes names, sizes, dates and the digests zeusd recorded.' : 'A preview of the contents needs the file itself. This release indexes sample names, sizes and dates only.'}</p>}
              </>
            ) : (
              <div class="pane-empty">Select a file to see its details.</div>
            )}
          </aside>
        ) : null}
      </div>
      {NATIVE ? <><CopyJobs /><ArchiveJobs onChanged={()=>setRevision(n=>n+1)} /></> : null}
      {archive ? <ArchiveDialog kind={archive.kind} source={archive.path} name={archive.name} directory={writeRoots[0] ?? parentOf(archive.path)} onClose={()=>setArchive(null)} /> : null}
      {insights ? <FileInsights roots={includedRoots} onBrowse={path=>{setFolder(path);setInsights(false)}} onClose={()=>setInsights(false)} onChanged={()=>setRevision(n=>n+1)} /> : null}
      <Provenance>
        {NATIVE
          ? 'The engine keeps this index of your download folders (and folders you add) and follows changes with the file system watcher. Folders are read directly from disk, including items excluded from search. Search everywhere runs over the index; it matches names, sizes and dates, not file contents. SHA-256, quarantine and the job behind each file come from zeusd. Zeus reads at most the start of a text file or a small picture for the preview, never a quarantined file.'
          : LIVE
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
            const f = visibleFiles.find((x) => x.id === menu.id)
            return f ? items(f) : []
          })()}
        />
      ) : null}
      {help ? <SyntaxHelp onClose={() => setHelp(false)} onUse={(q) => (setQuery(q), setHelp(false))} /> : null}
      {saving ? <SaveBookmark query={query} filter={filter} onClose={() => setSaving(false)} /> : null}
      {mkdir ? <NewFolderDialog parent={mkdir} onClose={()=>{setMkdir(null);setRevision(n=>n+1)}} /> : null}
      {op && opFile && op.kind === 'rename' ? <RenameDialog path={fullPath(opFile)} name={opFile.name} onClose={() => {setOp(null);setRevision(n=>n+1)}} /> : null}
      {op && opFile && (op.kind === 'move' || op.kind === 'copy') ? <MoveDialog copy={op.kind === 'copy'} path={fullPath(opFile)} dir={opFile.isFolder ? parentOf(fullPath(opFile)) : opFile.dir} roots={writeRoots} folders={moveFolders} onClose={() => {setOp(null);setRevision(n=>n+1)}} /> : null}
      {op && opFile && op.kind === 'delete' ? <DeleteDialog paths={[fullPath(opFile)]} names={[opFile.name]} onClose={() => {setOp(null);setRevision(n=>n+1)}} /> : null}
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


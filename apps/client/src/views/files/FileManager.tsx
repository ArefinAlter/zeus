// Both panes use the scoped native listing and operation routes. A comparison is a view of
// the loaded directory snapshots; it is never an instruction to replace or delete files.
import { useEffect, useRef, useState } from 'preact/hooks'
import { filesApi, type IndexRow } from '../../backend/files'
import { baseOf, isInside, parentOf, SEP } from '../../lib/paths'
import { href, navigate } from '../../lib/router'
import { fmtBytes, fmtStamp } from '../../model/units'
import { Notice } from '../../ui/components'
import { RecursiveComparison } from './RecursiveComparison'
import { CopyJobs, DeleteDialog, MoveDialog, NewFolderDialog, RenameDialog } from './FileOps'

const MAX_ROWS = 10_000
const ROW_HEIGHT = 32
const keyOf = (path: string) => SEP() === '\\' ? path.replaceAll('/', '\\').toLowerCase() : path
const samePath = (a: string, b: string) => keyOf(a) === keyOf(b)
const allowed = (roots: string[], path: string) => roots.some(root => isInside(root, path))

function useFolder(initial: string, roots: string[]) {
  const [path, setPath] = useState(initial)
  const [rows, setRows] = useState<IndexRow[]>([])
  const [next, setNext] = useState<string | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [skipped, setSkipped] = useState(0)
  const [ready, setReady] = useState(false)
  const [settledFor, setSettledFor] = useState('')
  const [loadedFor, setLoadedFor] = useState('')
  const [revision, setRevision] = useState(0)
  const epoch = useRef(0)
  const rootKey = JSON.stringify(roots)
  const requestKey = JSON.stringify([path, rootKey])
  const readable = !!path && allowed(roots, path)
  const load = async (cursor: string, generation: number) => {
    setBusy(true)
    setError('')
    try {
      const page = await filesApi.list(path, cursor)
      if (epoch.current !== generation) return
      if (!samePath(page.path, path)) throw new Error('The folder listing changed. Refresh this pane.')
      if (cursor && page.next === cursor) throw new Error('The listing cursor did not advance. Refresh this pane.')
      setRows(previous => {
        const merged = new Map((cursor ? previous : []).map(row => [keyOf(row.path), row]))
        for (const row of page.files) merged.set(keyOf(row.path), row)
        return [...merged.values()].slice(0, MAX_ROWS)
      })
      setNext(page.next)
      setSkipped(previous => (cursor ? previous : 0) + page.skipped)
      setLoadedFor(requestKey)
      setReady(true)
    } catch (e) {
      if (epoch.current === generation) setError(e instanceof Error ? e.message : 'This folder could not be read.')
    } finally {
      if (epoch.current === generation) { setBusy(false); setSettledFor(requestKey) }
    }
  }
  useEffect(() => {
    const start = initial || roots[0]
    if (!path && start) setPath(start)
  }, [rootKey, path, initial])
  useEffect(() => {
    const generation = ++epoch.current
    setRows([]); setNext(null); setSelected(null); setSkipped(0); setError(''); setReady(false); setBusy(false)
    if (readable) void load('', generation)
    else {
      if (path) setError('This folder is outside the currently included folders.')
      setSettledFor(requestKey)
    }
    return () => { epoch.current++ }
  }, [path, revision, rootKey])
  const visible = readable && loadedFor === requestKey ? rows : []
  const current = visible.find(row => row.path === selected) ?? null
  const loading = busy || settledFor !== requestKey
  return { path, setPath, rows: visible, next: loadedFor === requestKey ? next : null, selected, setSelected, current, busy: loading, error: settledFor === requestKey ? error : '', skipped: loadedFor === requestKey ? skipped : 0, readable,
    complete: ready && loadedFor === requestKey && !loading && !error && !next && skipped === 0,
    refresh: () => setRevision(n => n + 1),
    more: () => { if (!busy && next && rows.length < MAX_ROWS) void load(next, epoch.current) },
  }
}
type FolderState = ReturnType<typeof useFolder>
type Operation = { kind: 'copy' | 'move' | 'rename' | 'delete'; source: IndexRow; destination: string }

export function FileManager({ roots, writeRoots, initialPath, onClose }: {
  roots: string[]; writeRoots: string[]; initialPath?: string | null; onClose: () => void
}) {
  const left = useFolder(initialPath && allowed(roots, initialPath) ? initialPath : roots[0] ?? '', roots)
  const right = useFolder(roots.find(root => !samePath(root, left.path)) ?? roots[0] ?? '', roots)
  const [op, setOp] = useState<Operation | null>(null)
  const [mkdir, setMkdir] = useState<string | null>(null)
  const [compare, setCompare] = useState(false)
  const [active, setActive] = useState('Left')
  const [verification, setVerification] = useState<{ signature: string; text: string; busy: boolean } | null>(null)
  const verificationEpoch = useRef(0)
  const pair = [left.current, right.current]
  const signature = JSON.stringify(pair.map(row => row && [row.path, row.size, row.mtime]))
  useEffect(() => { verificationEpoch.current++; setVerification(null) }, [signature, left.path, right.path, left.busy, right.busy])
  useEffect(() => () => { verificationEpoch.current++ }, [])
  const refresh = () => { left.refresh(); right.refresh() }
  const closeOperation = () => { setOp(null); refresh() }
  const request = (kind: Operation['kind'], source: FolderState, destination: FolderState) => {
    if (source.current) setOp({ kind, source: { ...source.current }, destination: destination.path })
  }
  const verify = async () => {
    const paths = pair.flatMap(row => row ? [row.path] : [])
    if (paths.length !== 2 || pair.some(row => !row || row.is_dir || row.quarantined)) return
    const generation = ++verificationEpoch.current
    setVerification({ signature, text: 'Reading both selected files…', busy: true })
    try {
      const result = await filesApi.verifyDuplicates(paths)
      if (generation !== verificationEpoch.current) return
      const equal = result.groups.some(group => group.verified && paths.every(path => group.paths.some(other => samePath(path, other))))
      setVerification({ signature, text: equal ? 'Selected file contents match (SHA-256).' : 'Selected file contents differ.', busy: false })
    } catch (e) {
      if (generation === verificationEpoch.current) setVerification({ signature, text: e instanceof Error ? e.message : 'Content verification failed.', busy: false })
    }
  }
  const comparable = left.complete && right.complete
  const peerRows = (pane: FolderState) => new Map(pane.rows.map(row => [keyOf(baseOf(row.path)), row]))
  const leftNames = peerRows(left), rightNames = peerRows(right)
  const comparison = (row: IndexRow, peers: Map<string, IndexRow>): string => {
    const peer = peers.get(keyOf(baseOf(row.path)))
    if (!peer) return 'Only here'
    if (peer.is_dir !== row.is_dir) return 'Different type'
    if (row.is_dir) return 'Folder · not compared'
    return row.size === peer.size && row.mtime === peer.mtime ? 'Same metadata' : 'Different metadata'
  }
  return <div class="files-page file-manager">
    <div class="fm-toolbar">
      <h2>File manager</h2>
      <button class="btn small" onClick={onClose}>Search files</button>
      <button class={`btn small ${compare ? 'on' : ''}`} aria-pressed={compare} onClick={() => setCompare(value => !value)}>Compare folders</button>
      <button class="btn small" disabled={pair.some(row => !row || row.is_dir || row.quarantined) || samePath(pair[0]?.path ?? '', pair[1]?.path ?? '') || !!verification?.busy} onClick={() => void verify()}>Verify selected contents</button>
    </div>
    {!roots.length ? <Notice kind="warn">Choose included folders in Search and index settings to browse files.</Notice> : null}
    {compare ? <p class="small muted fm-explanation" role="status">{comparable ? 'Comparing the current folders by name, type, size and date. Subfolders and file contents are not compared.' : 'Load every page and resolve skipped entries or errors in both panes to compare these folders.'} Refresh after external changes.</p> : null}
    {verification?.signature === signature ? <p class="small fm-explanation" role="status">{verification.text}</p> : null}
    <div class="fm-pane-switch" role="group" aria-label="Visible file pane">
      {['Left', 'Right'].map(name => <button class={`btn small ${active === name ? 'on' : ''}`} aria-pressed={active === name} onClick={() => setActive(name)}>{name} pane</button>)}
    </div>
    <div class="fm-panes">
      <FolderPane name="Left" active={active === 'Left'} pane={left} other={right} roots={roots} writeRoots={writeRoots} comparison={compare && comparable ? row => comparison(row, rightNames) : undefined} request={kind => request(kind, left, right)} mkdir={() => setMkdir(left.path)} />
      <FolderPane name="Right" active={active === 'Right'} pane={right} other={left} roots={roots} writeRoots={writeRoots} comparison={compare && comparable ? row => comparison(row, leftNames) : undefined} request={kind => request(kind, right, left)} mkdir={() => setMkdir(right.path)} />
    </div>
    {compare ? <RecursiveComparison left={left.path} right={right.path} readable={left.readable && right.readable} /> : null}
    <CopyJobs />
    {op && (op.kind === 'copy' || op.kind === 'move') ? <MoveDialog copy={op.kind === 'copy'} path={op.source.path} dir={parentOf(op.source.path)} initialDestination={op.destination} roots={writeRoots} folders={[left.path, right.path]} onClose={closeOperation} /> : null}
    {op?.kind === 'rename' ? <RenameDialog path={op.source.path} name={baseOf(op.source.path)} onClose={closeOperation} /> : null}
    {op?.kind === 'delete' ? <DeleteDialog paths={[op.source.path]} names={[baseOf(op.source.path)]} onClose={closeOperation} /> : null}
    {mkdir ? <NewFolderDialog parent={mkdir} onClose={() => { setMkdir(null); refresh() }} /> : null}
  </div>
}

function FolderPane({ name, active, pane, other, roots, writeRoots, comparison, request, mkdir }: {
  name: string; active: boolean; pane: FolderState; other: FolderState; roots: string[]; writeRoots: string[];
  comparison?: (row: IndexRow) => string; request: (kind: Operation['kind']) => void; mkdir: () => void
}) {
  const [address, setAddress] = useState(pane.path)
  const [scroll, setScroll] = useState(0)
  const [height, setHeight] = useState(400)
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => { setAddress(pane.path); setScroll(0); if (list.current) list.current.scrollTop = 0 }, [pane.path])
  useEffect(() => {
    const el = list.current
    if (!el) return
    const observer = new ResizeObserver(() => setHeight(el.clientHeight))
    observer.observe(el)
    return () => observer.disconnect()
  }, [])
  const current = pane.current
  const sourceAllowed = !!current && pane.readable && !pane.busy && !pane.error && !current.quarantined
  const canWrite = sourceAllowed && allowed(writeRoots, parentOf(current.path))
  const canTransfer = sourceAllowed && other.readable && !other.busy && !other.error && allowed(writeRoots, other.path) && !samePath(parentOf(current.path), other.path) && !(current.is_dir && isInside(current.path, other.path))
  const first = Math.max(0, Math.floor(scroll / ROW_HEIGHT) - 5)
  const last = Math.min(pane.rows.length, Math.ceil((scroll + height) / ROW_HEIGHT) + 5)
  const open = (row: IndexRow) => { if (row.is_dir && !row.quarantined) pane.setPath(row.path) }
  const onKey = (event: KeyboardEvent) => {
    const index = pane.rows.findIndex(row => row.path === pane.selected)
    let target: number | undefined
    if (event.key === 'ArrowDown') target = Math.min(pane.rows.length - 1, index + 1)
    if (event.key === 'ArrowUp') target = Math.max(0, index - 1)
    if (event.key === 'Home') target = 0
    if (event.key === 'End') target = pane.rows.length - 1
    if (target !== undefined) {
      event.preventDefault()
      const row = pane.rows[target]
      if (!row) return
      pane.setSelected(row.path)
      if (list.current && (target * ROW_HEIGHT < list.current.scrollTop || (target + 1) * ROW_HEIGHT > list.current.scrollTop + height)) list.current.scrollTop = Math.max(0, target * ROW_HEIGHT - height / 2)
    } else if (event.key === 'Enter' && current) { event.preventDefault(); open(current) }
    else if (event.key === 'F2' && canWrite) { event.preventDefault(); request('rename') }
    else if (event.key === 'Delete' && canWrite) { event.preventDefault(); request('delete') }
    else if (event.key === 'F5' && canTransfer) { event.preventDefault(); request('copy') }
    else if (event.key === 'F6' && canTransfer && canWrite) { event.preventDefault(); request('move') }
  }
  return <section class={`fm-pane ${active ? 'active' : ''}`} aria-label={`${name} file pane`}>
    <div class="fm-pane-tools">
      <strong>{name}</strong>
      <button class="btn small" disabled={!allowed(roots, parentOf(pane.path)) || samePath(parentOf(pane.path), pane.path)} onClick={() => pane.setPath(parentOf(pane.path))}>Up</button>
      <button class="btn small" disabled={pane.busy || !pane.readable} onClick={pane.refresh}>Refresh</button>
      <button class="btn small" disabled={!pane.readable || !allowed(writeRoots, pane.path)} onClick={mkdir}>New folder</button>
      <select aria-label={`${name} included folder`} value={roots.includes(pane.path) ? pane.path : ''} onChange={event => { if (event.currentTarget.value) pane.setPath(event.currentTarget.value) }}>
        <option value="" disabled>Choose folder</option>
        {roots.map(root => <option key={root} value={root}>{root}</option>)}
      </select>
    </div>
    <form class="fm-address" onSubmit={event => { event.preventDefault(); if (address.trim()) pane.setPath(address.trim()) }}>
      <label><span class="sr-only">{name} folder path</span><input class="mono" value={address} onInput={event => setAddress(event.currentTarget.value)} /></label>
      <button type="submit" class="btn small">Go</button>
    </form>
    <div class="fm-transfer">
      <button class="btn small" disabled={!canTransfer} title="Review the destination (F5)" onClick={() => request('copy')}>Copy to {name === 'Left' ? 'right' : 'left'}…</button>
      <button class="btn small" disabled={!canTransfer || !canWrite} title="Review the destination (F6)" onClick={() => request('move')}>Move to {name === 'Left' ? 'right' : 'left'}…</button>
      <button class="btn small" disabled={!canWrite} onClick={() => request('rename')}>Rename…</button>
      <button class="btn small" disabled={!canWrite} onClick={() => request('delete')}>Delete…</button>
      <button class="btn small" disabled={!sourceAllowed || current.is_dir} onClick={() => current && navigate(href('uploads', { path: current.path, name: baseOf(current.path) }))}>Upload…</button>
    </div>
    <div class="fm-list" ref={list} tabIndex={0} role="grid" aria-label={`${name} folder contents`} aria-busy={pane.busy} data-folder={pane.path} aria-rowcount={pane.rows.length + 1} aria-colcount={comparison ? 4 : 3} aria-activedescendant={pane.rows.slice(first, last).some(row => row.path === pane.selected) ? `fm-${name}-${pane.rows.findIndex(row => row.path === pane.selected)}` : undefined} onKeyDown={onKey} onScroll={event => setScroll(event.currentTarget.scrollTop)}>
      <div class={`fm-row fm-columns ${comparison ? 'comparing' : ''}`} role="row" aria-rowindex={1}><span role="columnheader">Name</span><span role="columnheader">Size</span><span role="columnheader">Modified</span>{comparison ? <span role="columnheader">Comparison</span> : null}</div>
      <div class="fm-rows" style={{ height: pane.rows.length * ROW_HEIGHT }}>
        {pane.rows.slice(first, last).map((row, offset) => <div key={row.path} id={`fm-${name}-${first + offset}`} class={`fm-row ${comparison ? 'comparing' : ''} ${pane.selected === row.path ? 'selected' : ''}`} role="row" aria-rowindex={first + offset + 2} aria-selected={pane.selected === row.path} style={{ top: (first + offset) * ROW_HEIGHT }} onClick={() => { pane.setSelected(row.path); list.current?.focus() }} onDblClick={() => open(row)}>
          <span role="gridcell" title={row.path}>{row.is_dir ? '▸ ' : ''}{baseOf(row.path)}{row.quarantined ? ' · quarantined' : ''}</span>
          <span role="gridcell">{row.is_dir ? '—' : fmtBytes(row.size)}</span>
          <span role="gridcell" title={fmtStamp(row.mtime)}>{fmtStamp(row.mtime)}</span>
          {comparison ? <span role="gridcell" title={comparison(row)}>{comparison(row)}</span> : null}
        </div>)}
      </div>
      {!pane.rows.length && !pane.busy && !pane.error && pane.readable ? <p class="muted small">This folder is empty.</p> : null}
    </div>
    <div class="fm-pane-status" role="status">
      <span>{pane.busy ? 'Reading folder…' : `${pane.rows.length.toLocaleString()} items loaded`}{pane.skipped ? ` · ${pane.skipped} entries skipped` : ''}</span>
      {pane.next && pane.rows.length < MAX_ROWS ? <button class="btn small" disabled={pane.busy} onClick={pane.more}>Load more</button> : null}
      {pane.next && pane.rows.length >= MAX_ROWS ? <span>10,000-item view limit. Use Search files for this folder.</span> : null}
      {pane.error ? <span class="field-err">{pane.error}</span> : null}
    </div>
  </section>
}

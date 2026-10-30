// Native folders are loaded independently of the bounded search page. Only expanded nodes
// enumerate their immediate directories, retaining the backend's read grants and pagination.
import { useEffect, useRef, useState } from 'preact/hooks'
import { filesApi } from '../../backend/files'
import { baseOf } from '../../lib/paths'
import { Icon } from '../../ui/icons'

type Selection = { selected: string | null; revision: string; onSelect: (path: string) => void }

function FolderNode({ path, depth, selected, revision, onSelect }: Selection & { path: string; depth: number }) {
  const [open, setOpen] = useState(depth === 0)
  const [children, setChildren] = useState<string[]>([])
  const [next, setNext] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [failed, setFailed] = useState(false)
  const [failedAfter, setFailedAfter] = useState('')
  const epoch = useRef(0)
  const loading = useRef(false)
  const load = async (after: string, request: number) => {
    if (loading.current) return
    loading.current = true
    setBusy(true)
    setFailed(false)
    try {
      const page = await filesApi.list(path, after, true)
      if (epoch.current !== request) return
      const folders = page.files.filter(row => row.is_dir).map(row => row.path)
      setChildren(previous => [...new Set([...(after ? previous : []), ...folders])])
      setNext(page.next)
    } catch {
      if (epoch.current === request) { setFailedAfter(after); setFailed(true) }
    } finally {
      if (epoch.current === request) {
        loading.current = false
        setBusy(false)
      }
    }
  }
  useEffect(() => {
    const request = ++epoch.current
    loading.current = false
    if (!open || depth >= 64) return
    // Coalesce watcher updates while a directory is changing.
    const timer = setTimeout(() => void load('', request), 200)
    return () => { clearTimeout(timer); epoch.current++ }
  }, [path, open, revision])

  return <li>
    <div style={{ display: 'flex', paddingLeft: `${depth * 14}px` }}>
      <button type="button" style={{ width: '26px', flexShrink: 0 }} aria-label={`${open ? 'Collapse' : 'Expand'} ${path}`} aria-expanded={open} onClick={() => setOpen(!open)}>{open ? '−' : '+'}</button>
      <button type="button" class={selected === path ? 'on' : ''} title={path} onClick={() => { onSelect(path); setOpen(true) }}>
        <Icon name={depth === 0 ? 'drive' : open ? 'folderopen' : 'folder'} />
        <span class="name">{depth === 0 ? path : baseOf(path)}</span>
      </button>
    </div>
    {open ? <ul class="ftree">
      {children.map(child => <FolderNode key={child} path={child} depth={depth + 1} selected={selected} revision={revision} onSelect={onSelect} />)}
      {busy ? <li role="status">Reading folders…</li> : null}
      {failed ? <li role="status">The folder could not be read. <button type="button" onClick={() => void load(failedAfter, epoch.current)}>Retry</button></li> : null}
      {next ? <li><button type="button" disabled={busy} aria-label={`Load more folders in ${path}`} onClick={() => void load(next, epoch.current)}>Load more folders</button></li> : null}
    </ul> : null}
  </li>
}

export function NativeFolderTree({ roots, ...selection }: Selection & { roots: string[] }) {
  return <ul class="ftree">{roots.map(path => <FolderNode key={path} path={path} depth={0} {...selection} />)}</ul>
}

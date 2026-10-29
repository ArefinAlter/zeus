// The dialogs of the file operations: rename, move and delete. They act on a file inside the download folders at the
// person's click; the engine's Rust side checks every path again and refuses anything outside or quarantined.

import { useEffect, useState } from 'preact/hooks'

import { filesApi, type CopyJob } from '../../backend/files'
import { nudgeIndexSync } from '../../backend/files'
import { toast } from '../../backend/store'
import { fmtBytes } from '../../model/units'
import { SEP, joinPath } from '../../lib/paths.ts'
import { Dialog, Notice } from '../../ui/components'

const BAD = /[\\/:*?"<>|\u0000-\u001f]/

/** What is wrong with a file name, in words, or null. */
export function nameFault(name: string): string | null {
  if (!name.trim()) return 'Type a name.'
  if (name.length > 255) return 'A name has at most 255 characters.'
  if (name === '.' || name === '..') return 'That is not a file name.'
  if (BAD.test(name)) return 'A name cannot contain / \\ : * ? " < > | or control characters.'
  if (/[. ]$/.test(name)) return 'A name cannot end with a dot or a space.'
  return null
}

const done = (title: string, desc: string) => {
  toast({ kind: 'ok', title, desc, important: false, topic: 'system' })
  nudgeIndexSync()
}

export function RenameDialog({ path, name, onClose }: { path: string; name: string; onClose: () => void }) {
  const [value, setValue] = useState(name)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const fault = nameFault(value)
  const save = () => {
    if (fault || value === name) return
    setBusy(true)
    filesApi.rename(path, value).then(
      () => {
        done('Renamed', value)
        onClose()
      },
      (e: unknown) => {
        setBusy(false)
        setError(e instanceof Error ? e.message : 'The file could not be renamed.')
      },
    )
  }
  return (
    <Dialog title="Rename" onClose={onClose}>
      <label class="field">
        Name
        <input
          value={value}
          autofocus
          onInput={(e) => setValue((e.target as HTMLInputElement).value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') save()
          }}
          onFocus={(e) => {
            const el = e.target as HTMLInputElement
            const dot = el.value.lastIndexOf('.')
            el.setSelectionRange(0, dot > 0 ? dot : el.value.length)
          }}
        />
      </label>
      {value !== name && fault ? <p class="field-err">{fault}</p> : null}
      <p class="muted small">A name that exists already is refused: Zeus never replaces a file.</p>
      {error ? <Notice kind="error">{error}</Notice> : null}
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        <button type="button" class="btn primary" disabled={busy || !!fault || value === name} onClick={save}>
          Rename
        </button>
      </div>
    </Dialog>
  )
}

export function MoveDialog({ path, dir, roots, folders, onClose, copy = false, initialDestination }: { path: string; dir: string; roots: string[]; folders: string[]; onClose: () => void; copy?: boolean; initialDestination?: string }) {
  const [value, setValue] = useState(initialDestination ?? dir)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [job, setJob] = useState<CopyJob | null>(null)
  useEffect(() => {
    if (!job || !['scanning', 'copying'].includes(job.state)) return
    let alive = true
    const timer = setInterval(() => {
      void filesApi.copyStatus(job.id).then(next => {
        if (!alive) return
        setJob(next)
        if (next.state === 'complete') { done('Copied', next.path); onClose() }
        else if (next.state === 'failed' || next.state === 'cancelled') { setBusy(false); setError(next.error || 'The copy stopped. Completed destination files are kept.') }
      }, () => { if (alive) setError('Progress is unavailable. The copy may still be running.') })
    }, 350)
    return () => { alive = false; clearInterval(timer) }
  }, [job?.id, job?.state])
  const save = () => {
    setBusy(true)
    ;(copy ? filesApi.copyStart(path, value.trim()) : filesApi.move(path, value.trim())).then(
      (result) => {
        if (copy) { setJob(result as CopyJob); return }
        done('Moved', value.trim())
        onClose()
      },
      (e: unknown) => {
        setBusy(false)
        setError(e instanceof Error ? e.message : 'The file could not be moved.')
      },
    )
  }
  return (
    <Dialog title={copy ? "Copy to a folder" : "Move to a folder"} onClose={onClose}>
      <p class="small break">Source: <span class="mono">{path}</span></p>
      <label class="field">
        Folder
        <input
          class="mono"
          list="move-folders"
          value={value}
          onInput={(e) => setValue((e.target as HTMLInputElement).value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && value.trim() && value.trim() !== dir) save()
          }}
        />
        <datalist id="move-folders">
          {[...new Set([...roots, ...folders])].slice(0, 300).map((f) => (
            <option value={f} />
          ))}
        </datalist>
      </label>
      <p class="muted small">
        Inside a folder enabled for file management ({roots.join(', ') || 'none'}). {copy ? 'Choose an existing folder. Folder copies include all regular files, including hidden items; links and junctions are refused.' : 'Choose an existing destination folder. Same-drive moves have guarded undo in Storage and history; use Copy for another drive.'} An existing destination of the same name is refused; Zeus never replaces a file. Example: {joinPath(roots[0] ?? '', 'Sorted')}.
      </p>
      {job ? <p role="status">{job.state} · {fmtBytes(job.bytes)} / {fmtBytes(job.total_bytes)} · {job.files} files{job.path ? <><br /><span class="mono">{job.path}</span></> : null}</p> : null}
      {error ? <Notice kind="error">{error}</Notice> : null}
      <div class="decide-row">
        <button type="button" class="btn" onClick={() => { if (busy && job) void filesApi.copyCancel(job.id).then(setJob); else onClose() }}>
          {busy && job ? 'Cancel copy' : 'Cancel'}
        </button>
        <button type="button" class="btn primary" disabled={busy || !value.trim() || value.trim() === dir} onClick={save}>
          {copy ? "Copy" : "Move"}
        </button>
      </div>
    </Dialog>
  )
}

export function DeleteDialog({ paths, names, onClose }: { paths: string[]; names: string[]; onClose: () => void }) {
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [offerPermanent, setOfferPermanent] = useState(false)
  const bin = SEP() === '\\' ? 'Recycle Bin' : 'Trash'
  const run = (permanent: boolean) => {
    setBusy(true)
    filesApi.remove(paths, permanent).then(
      (r) => {
        if (r.failed.length && !permanent && r.failed.every((f) => f.code === 'app.trash_unavailable')) {
          setBusy(false)
          setOfferPermanent(true)
          setError(r.failed[0]?.message ?? `The ${bin} is not available here.`)
          return
        }
        if (r.failed.length) {
          setBusy(false)
          setError(r.failed[0]?.message ?? 'Some items could not be deleted.')
        } else {
          done(permanent ? 'Deleted' : `Moved to the ${bin}`, names.length === 1 ? (names[0] ?? '') : `${names.length} items`)
          onClose()
        }
      },
      (e: unknown) => {
        setBusy(false)
        setError(e instanceof Error ? e.message : 'The files could not be deleted.')
      },
    )
  }
  return (
    <Dialog title={names.length === 1 ? 'Delete this file?' : `Delete ${names.length} items?`} onClose={onClose}>
      <p class="mono small break">{names.slice(0, 5).join('\n')}{names.length > 5 ? `\n… and ${names.length - 5} more` : ''}</p>
      <p class="muted small">
        {offerPermanent ? `Deleting for good cannot be undone. The ${bin} is not available here.` : `They go to the ${bin}, so you can get them back. Zeus keeps its record of the download.`}
      </p>
      {error ? <Notice kind="error">{error}</Notice> : null}
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        {offerPermanent ? (
          <button type="button" class="btn danger" disabled={busy} onClick={() => run(true)}>
            Delete for good
          </button>
        ) : (
          <button type="button" class="btn primary" disabled={busy} onClick={() => run(false)}>
            Move to the {bin}
          </button>
        )}
      </div>
    </Dialog>
  )
}

export function NewFolderDialog({ parent, onClose }: { parent: string; onClose: () => void }) {
  const [name,setName]=useState('');const [error,setError]=useState('');const [busy,setBusy]=useState(false)
  const save=()=>{if(nameFault(name))return;setBusy(true);filesApi.mkdir(parent,name).then(()=>{done('Folder created',name);onClose()},e=>{setBusy(false);setError(e instanceof Error?e.message:'The folder could not be created.')})}
  return <Dialog title="New folder" onClose={onClose}><p class="mono">{parent}</p><label class="field">Name<input value={name} autofocus onInput={e=>setName(e.currentTarget.value)} onKeyDown={e=>{if(e.key==='Enter')save()}}/></label>{error?<Notice kind="error">{error}</Notice>:null}<div class="decide-row"><button class="btn" onClick={onClose}>Cancel</button><button class="btn primary" disabled={busy || !!nameFault(name)} onClick={save}>Create folder</button></div></Dialog>
}

/** Session copy progress remains available after a destination dialog closes. */
export function CopyJobs() {
  const [jobs, setJobs] = useState<CopyJob[]>([])
  const [hidden, setHidden] = useState<string[]>([])
  const [error, setError] = useState('')
  useEffect(() => {
    let alive = true
    const refresh = () => void filesApi.copies().then(r => {if(alive) setJobs(r.jobs)}, () => undefined)
    refresh()
    const timer = setInterval(refresh, 1000)
    return () => {alive=false;clearInterval(timer)}
  }, [])
  const shown = jobs.filter(job=>!hidden.includes(job.id))
  if (!shown.length) return null
  return <div class="copy-jobs" aria-label="File copies">{error ? <Notice kind="error">{error}</Notice> : null}{shown.map(job=><div class="fscope" key={job.id}>
    <span role="status">{job.state} · {fmtBytes(job.bytes)} / {fmtBytes(job.total_bytes)} · {job.files} files<br /><span class="mono">{job.path || 'Scanning source…'}</span>{job.error ? <><br />{job.error}</> : null}</span>
    {job.state==='scanning' || job.state==='copying' ? <button class="btn small" onClick={()=>void filesApi.copyCancel(job.id).catch(e=>setError(e instanceof Error ? e.message : 'Cancellation could not be requested.'))}>Cancel copy</button> : <button class="btn small ghost" onClick={()=>setHidden(ids=>[...ids,job.id])}>Dismiss</button>}
  </div>)}</div>
}

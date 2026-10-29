import { useEffect, useRef, useState } from 'preact/hooks'
import { filesApi, type ComparisonRow, type FolderComparison } from '../../backend/files'
import { fmtBytes } from '../../model/units'
import { Notice } from '../../ui/components'

const labels: Record<ComparisonRow['status'], string> = {
  left_only: 'Only on the left', right_only: 'Only on the right', type_conflict: 'Different file types',
  directory: 'Directory · contents listed below', content_match: 'Content matches · SHA-256',
  content_different: 'Content differs · SHA-256', metadata_match: 'Metadata matches · contents unchecked',
  metadata_different: 'Metadata differs · contents unchecked',
}

export function RecursiveComparison({ left, right, readable }: { left: string; right: string; readable: boolean }) {
  const [content, setContent] = useState(false)
  const [job, setJob] = useState<FolderComparison | null>(null)
  const [rows, setRows] = useState<ComparisonRow[]>([])
  const [next, setNext] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const epoch = useRef(0)
  const active = useRef<string | null>(null)
  useEffect(() => {
    epoch.current++
    if (active.current) void filesApi.compareCancel(active.current).catch(() => undefined)
    active.current = null
    setJob(null); setRows([]); setNext(null); setBusy(false); setError('')
    return () => { epoch.current++; if (active.current) void filesApi.compareCancel(active.current).catch(() => undefined) }
  }, [left, right])
  const page = async (id: string, offset: number, generation: number) => {
    const result = await filesApi.comparePage(id, offset)
    if (epoch.current !== generation) return
    if (!result.complete) throw new Error('This comparison is incomplete. Scan again before planning changes.')
    setRows(result.items); setNext(result.next)
  }
  useEffect(() => {
    if (job?.state !== 'running') return
    const generation = epoch.current
    let pending = false
    const timer = setInterval(() => {
      if (pending) return
      pending = true
      void filesApi.compareStatus(job.id).then(async current => {
        if (epoch.current !== generation) return
        setJob(current)
        if (current.state !== 'running') active.current = null
        if (current.complete) await page(current.id, 0, generation)
      }).catch(e => { if (epoch.current === generation) setError(e instanceof Error ? e.message : 'Comparison status is unavailable.') }).finally(() => { pending = false })
    }, 500)
    return () => clearInterval(timer)
  }, [job?.id, job?.state])
  const start = async () => {
    const generation = ++epoch.current
    setBusy(true); setError(''); setRows([]); setNext(null)
    try {
      const result = await filesApi.compareStart(left, right, content)
      if (epoch.current !== generation) { void filesApi.compareCancel(result.id); return }
      active.current = result.id
      setJob(result)
    } catch (e) { if (epoch.current === generation) setError(e instanceof Error ? e.message : 'The folders could not be compared.') }
    finally { if (epoch.current === generation) setBusy(false) }
  }
  return <section class="fm-recursive" aria-label="Recursive folder comparison">
    <div class="fm-toolbar">
      <strong>Compare including subfolders</strong>
      <label><input type="checkbox" checked={content} disabled={busy || job?.state === 'running'} onChange={e => setContent(e.currentTarget.checked)} /> Verify file contents with SHA-256</label>
      <button class="btn small" disabled={!readable || left === right || busy || job?.state === 'running'} onClick={() => void start()}>{busy ? 'Starting…' : 'Scan both folders'}</button>
      {job?.state === 'running' ? <button class="btn small" onClick={() => void filesApi.compareCancel(job.id).catch(e => setError(String(e)))}>Cancel comparison</button> : null}
    </div>
    <p class="small muted">Up to 100,000 entries and 128 levels across both folders. Content verification reads up to 8 GiB; each scan has a two-minute limit. Links, unreadable entries and recovery folders make the result incomplete. This view does not change files.</p>
    {error || job?.error ? <Notice kind="error">{error || job?.error}</Notice> : null}
    {job ? <p class="small" role="status">{job.state} · {job.entries} entries · {fmtBytes(job.hashed_bytes)} hashed{job.complete ? ` · ${job.rows} comparison rows` : ''}</p> : null}
    {rows.length ? <div class="fm-comparison-rows" role="list" aria-label="Recursive comparison results">{rows.map(row => <div class="fm-comparison-row" role="listitem" key={row.relative}><span class="mono break">{row.relative}</span><span>{labels[row.status]}</span></div>)}</div> : null}
    {job?.complete && next !== null ? <button class="btn small" onClick={() => void page(job.id, next, epoch.current).catch(e => setError(e instanceof Error ? e.message : 'This page is unavailable.'))}>Next 200 results</button> : null}
  </section>
}

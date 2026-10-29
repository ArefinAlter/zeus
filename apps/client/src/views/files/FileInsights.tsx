// Native scoped index summaries and durable file-operation receipts. None of these controls grants
// agent authority; undo rechecks current writable roots, quarantine facts and file identity in Rust.
import { useEffect, useState } from 'preact/hooks'
import { filesApi, nudgeIndexSync, type FileAnalytics, type FileHistoryRecord } from '../../backend/files'
import { fmtBytes, fmtStamp } from '../../model/units'
import { Dialog, Notice } from '../../ui/components'
import { EverythingIntegration } from './Everything'

export function FileInsights({ onClose, onChanged, roots, onBrowse }: { onClose: () => void; onChanged: () => void; roots: string[]; onBrowse: (path: string) => void }) {
  const [tab, setTab] = useState<'sizes' | 'duplicates' | 'history' | 'everything'>('sizes')
  const [analytics, setAnalytics] = useState<FileAnalytics | null>(null)
  const [history, setHistory] = useState<FileHistoryRecord[]>([])
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const [verified, setVerified] = useState<{ sha256: string; paths: string[] }[]>([])
  const reload = async () => {
    const [a, h] = await Promise.all([filesApi.analytics(), filesApi.history()])
    setAnalytics(a); setHistory(h.records)
    if (h.load_failed) setError('Operation history could not be read. Preserve the file for recovery; tracked moves are blocked.')
  }
  useEffect(() => { let alive=true; void reload().catch(e=>{if(alive)setError(e instanceof Error?e.message:'The file workspace did not answer.')});return()=>{alive=false} }, [])
  const act = async (record: FileHistoryRecord) => {
    setBusy(true); setError(''); setMessage('')
    try {
      if (record.can_undo) await filesApi.undo(record.id)
      else await filesApi.reconcile(record.id)
      setMessage(record.can_undo ? 'Move undone. The original path was free and the item identity matched.' : 'Receipt reconciled from the two paths. No file was moved.')
      nudgeIndexSync(); onChanged(); await reload()
    } catch (e) { setError(e instanceof Error ? e.message : 'The operation could not be completed.') }
    finally { setBusy(false) }
  }
  const verify = async (paths: string[]) => {
    setBusy(true);setError('');setMessage('');setVerified([])
    try { const result=await filesApi.verifyDuplicates(paths);setVerified(result.groups);setMessage(`${fmtBytes(result.bytes_read)} read. ${result.groups.length} verified duplicate group${result.groups.length===1?'':'s'}. Files are unchanged.`) }
    catch(e){setError(e instanceof Error?e.message:'The files could not be verified.')}
    finally{setBusy(false)}
  }
  return <Dialog title="Storage and operation history" onClose={onClose}>
    <div class="file-insight-tabs" role="group" aria-label="File workspace tools">
      {(['sizes','duplicates','history','everything'] as const).map(t=><button class={`btn small ${tab===t?'primary':''}`} aria-pressed={tab===t} onClick={()=>setTab(t)}>{t==='sizes'?'Storage':t==='duplicates'?'Duplicates':t==='history'?'Move and trash history':'Everything'}</button>)}
    </div>
    {error ? <Notice kind="error">{error}</Notice> : null}
    {message ? <p role="status">{message}</p> : null}
    <div class="file-insights">
      {tab==='everything' ? <EverythingIntegration roots={roots} onBrowse={onBrowse} /> : tab==='sizes' ? <>
        <p>{analytics ? `${analytics.files.toLocaleString()} indexed files · ${fmtBytes(analytics.logical_bytes)} logical size` : 'Reading scoped index…'}</p>
        <p class="muted small">Sizes cover indexed files in included folders. Sparse files, hard links and compression can make allocated disk usage differ. Parent folder totals overlap.</p>
        <h3>Largest folders</h3>
        {analytics?.directories.map(d=><div class="file-insight-row"><span class="mono">{d.path}</span><strong>{fmtBytes(d.bytes)}</strong><span>{d.files.toLocaleString()} files</span></div>)}
        <h3>File types</h3>
        {analytics?.extensions.map(d=><div class="file-insight-row"><span>{d.path}</span><strong>{fmtBytes(d.bytes)}</strong><span>{d.files.toLocaleString()} files</span></div>)}
      </> : tab==='duplicates' ? <>
        <p class="muted small">Equal sizes are candidates. Verify reads up to 32 selected files and 1 GiB, checks SHA-256 and refuses changed or quarantined sources. Files remain untouched.</p>
        {analytics?.duplicate_candidates.length===0 ? <p>No equal-size candidates above 1 KiB.</p> : null}
        {analytics?.duplicate_candidates.map(c=><div class="file-duplicate-candidate"><strong>{c.files} files · {fmtBytes(c.bytes)} each</strong>{c.truncated?<span class="muted"> · first 32 paths</span>:null}<ul>{c.paths.map(path=><li class="mono">{path}</li>)}</ul><button class="btn small" disabled={busy || c.bytes*c.paths.length>1024**3} onClick={()=>void verify(c.paths)}>Verify content{c.bytes*c.paths.length>1024**3?' · exceeds 1 GiB limit':''}</button></div>)}
        {verified.map(g=><div class="file-duplicate-candidate"><strong>Verified identical SHA-256</strong><p class="mono">{g.sha256}</p><ul>{g.paths.map(path=><li class="mono">{path}</li>)}</ul></div>)}
      </> : <>
        <p class="muted small">Rename, move and Zeus trash receipts survive restart. Trash stays in the managed folder’s .zeus/trash directory and still uses disk space; Undo restores its original path. Listed file sizes are logical bytes captured at the move, and folder totals are not counted here. No automatic expiry or purge runs. Undo refuses changed or replaced items, occupied paths, revoked folders and quarantine. Copy jobs and permanent deletions have separate lifecycles.</p>
        {history.length===0 ? <p>No tracked moves or trashed items on this device.</p> : null}
        {history.map(r=><div class="file-duplicate-candidate"><span>{fmtStamp(r.created_ms)} · {r.trash?'Zeus trash · ':''}{r.state} · {r.directory?'folder (contents not included in this receipt size)':fmtBytes(r.bytes)}</span><p class="mono">{r.from}<br />→ {r.path}</p>{r.can_undo || r.state==='pending' || r.state==='undoing' ? <button class="btn small" disabled={busy} onClick={()=>void act(r)}>{r.can_undo?'Undo / restore':'Reconcile receipt'}</button>:null}</div>)}
      </>}
    </div>
    <div class="decide-row"><button class="btn" disabled={busy} onClick={()=>void reload().catch(e=>setError(e instanceof Error?e.message:'Refresh failed.'))}>Refresh</button><button class="btn primary" onClick={onClose}>Close</button></div>
  </Dialog>
}

import { useEffect, useState } from 'preact/hooks'
import { filesApi, nudgeIndexSync, type ArchiveJob, type ArchivePlan } from '../../backend/files'
import { fmtBytes } from '../../model/units'
import { Dialog, Notice } from '../../ui/components'
import { nameFault } from './FileOps'

export function ArchiveDialog({kind,source,name:initialName,directory:initialDirectory,onClose}:{kind:'create'|'extract';source:string;name:string;directory:string;onClose:()=>void}) {
  const [name,setName]=useState(initialName)
  const [directory,setDirectory]=useState(initialDirectory)
  const [plan,setPlan]=useState<ArchivePlan|null>(null)
  const [error,setError]=useState('')
  const [busy,setBusy]=useState(false)
  const prepare=async()=>{setBusy(true);setError('');try{setPlan(await filesApi.archivePrepare(kind,source,directory,name))}catch(e){setError(e instanceof Error?e.message:'Preview failed.')}finally{setBusy(false)}}
  const start=async()=>{if(!plan)return;setBusy(true);setError('');try{await filesApi.archiveStart(plan.id);onClose()}catch(e){setError(e instanceof Error?e.message:'The archive could not start.');setBusy(false)}}
  const edit=()=>{if(plan)void filesApi.archiveDismiss(plan.id).catch(()=>undefined);setPlan(null)}
  return <Dialog title={kind==='create'?'Create ZIP archive':'Extract ZIP archive'} onClose={onClose}>
    <p class="mono">{source}</p>
    <label class="field">Destination folder<input value={directory} disabled={busy||!!plan} onInput={e=>setDirectory(e.currentTarget.value)} /></label>
    <label class="field">{kind==='create'?'ZIP filename':'New destination folder'}<input value={name} disabled={busy||!!plan} onInput={e=>setName(e.currentTarget.value)} /></label>
    <p class="muted small">Stored and Deflate ZIPs; up to 10,000 entries and 4 GiB of content. Extraction accepts ZIP inputs up to 1 GiB. Links, encrypted/split ZIPs and conflicting or unsafe paths are refused. Existing targets are never replaced.</p>
    {error?<Notice kind="error">{error}</Notice>:null}
    {plan?<><p>{plan.total_entries.toLocaleString()} entries · {fmtBytes(plan.total_bytes)} · <span class="mono">{plan.target}</span></p><ul class="archive-preview">{plan.entries.map(e=><li class="mono">{e.name}{e.directory?'/':` · ${fmtBytes(e.bytes)}`}</li>)}</ul>{plan.truncated?<p class="muted small">The first 100 entries are shown. Execution uses the complete saved manifest.</p>:null}</>:null}
    <div class="decide-row"><button class="btn" disabled={busy} onClick={plan?edit:onClose}>{plan?'Edit plan':'Cancel'}</button><button class="btn primary" disabled={busy||!!nameFault(name)||!directory.trim()} onClick={()=>void (plan?start():prepare())}>{busy?'Working…':plan?(kind==='create'?'Create ZIP':'Extract into new folder'):'Preview manifest'}</button></div>
  </Dialog>
}

export function ArchiveJobs({onChanged}:{onChanged:()=>void}) {
  const [jobs,setJobs]=useState<ArchiveJob[]>([])
  const [error,setError]=useState('')
  useEffect(()=>{let alive=true;const refresh=()=>void filesApi.archives().then(r=>{if(alive)setJobs(r.jobs)},()=>undefined);refresh();const timer=setInterval(refresh,1500);return()=>{alive=false;clearInterval(timer)}},[])
  const action=async(job:ArchiveJob,kind:'cancel'|'cleanup'|'dismiss'|'restart'|'reconcile')=>{
    try{if(kind==='cancel')await filesApi.archiveCancel(job.id);else if(kind==='cleanup')await filesApi.archiveCleanup(job.id);else if(kind==='restart')await filesApi.archiveStart(job.id);else if(kind==='reconcile')await filesApi.archiveReconcile(job.id);else await filesApi.archiveDismiss(job.id);setJobs((await filesApi.archives()).jobs);setError('');nudgeIndexSync();onChanged()}
    catch(e){setError(e instanceof Error?e.message:'The archive action failed.')}
  }
  if(!jobs.length&&!error)return null
  return <section class="archive-jobs" aria-label="Archive jobs">
    {error?<p class="field-err" role="alert">{error}</p>:null}
    {jobs.map(j=><div class="archive-job"><span><strong>{j.kind==='create'?'Create ZIP':'Extract ZIP'} · {j.state}</strong> · {fmtBytes(j.bytes)} / {fmtBytes(j.total_bytes)} · {j.entries}/{j.total_entries}<br /><span class="mono">{j.target}</span>{j.error?<span class="field-err"><br />{j.error}</span>:null}{j.sha256?<span class="muted small"><br />{j.digest_kind==='zip_bytes_sha256'?'ZIP SHA-256':'Extracted-file manifest SHA-256'}: <span class="mono">{j.sha256}</span></span>:null}</span><span class="archive-job-actions">{j.state==='running'||j.state==='publishing'?<button class="btn small" onClick={()=>void action(j,'cancel')}>Cancel</button>:<>{['uncertain','interrupted','failed'].includes(j.state)&&j.sha256?<button class="btn small" onClick={()=>void action(j,'reconcile')}>Reconcile published output</button>:null}{j.staging?<button class="btn small" onClick={()=>void action(j,'cleanup')}>Clean private staging</button>:null}{['ready','cancelled','failed','interrupted'].includes(j.state)&&!j.staging?<button class="btn small" onClick={()=>void action(j,'restart')}>{j.state==='ready'?'Start saved plan':'Recheck and restart'}</button>:null}{!j.staging&&j.state!=='uncertain'?<button class="btn small ghost" onClick={()=>void action(j,'dismiss')}>Dismiss receipt</button>:null}</>}</span></div>)}
  </section>
}

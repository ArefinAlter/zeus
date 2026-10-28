import { useEffect, useRef, useState } from 'preact/hooks'
import { NATIVE } from '../backend/live'
import { inboxApi, type InboxPackage, type PackagePage, type InboxPreview, type InboxSubmission } from '../backend/inbox'
import { href } from '../lib/router'
import { Notice, PageHead } from '../ui/components'
import '../styles/inbox.css'

export function Inbox() {
  const [packages,setPackages]=useState<InboxPackage[]>([])
  const [cursor,setCursor]=useState<string|null>(null)
  const [archived,setArchived]=useState(false)
  const [current,setCurrent]=useState<PackagePage|null>(null)
  const [name,setName]=useState(''),[dest,setDest]=useState(''),[text,setText]=useState('')
  const [selected,setSelected]=useState<string[]>([])
  const [preview,setPreview]=useState<InboxPreview|null>(null)
  const [receipt,setReceipt]=useState<InboxSubmission|null>(null)
  const [busy,setBusy]=useState(false),[error,setError]=useState(''),[message,setMessage]=useState('')
  // Retry bindings stay in memory. Durable recovery is supplied by the daemon, never localStorage.
  const keys=useRef(new Map<string,string>())
  const mutate=<T,>(operation:string,params:Record<string,unknown>)=>{
    const hash=JSON.stringify([operation,params]);let key=keys.current.get(hash)
    if(!key){key=`inbox-ui-${crypto.randomUUID()}`;keys.current.set(hash,key)}
    return inboxApi.request<T>(operation,{...params,idempotency_key:key})
  }
  const refresh=async(id=current?.package.id,append=false)=>{
    const list=await inboxApi.list(append?cursor??undefined:undefined,archived)
    setPackages(old=>append?[...old,...list.items]:list.items);setCursor(list.next_cursor)
    if(id){const value=await inboxApi.get(id);setCurrent(value);setName(value.package.untrusted_name);setDest(value.package.default_dest_dir??'')}
  }
  const run=(action:()=>Promise<void>)=>{setBusy(true);setError('');setMessage('');void action().catch(e=>setError(e instanceof Error?e.message:'The inbox request failed.')).finally(()=>setBusy(false))}
  useEffect(()=>{if(NATIVE)run(()=>refresh())},[archived])
  const save=()=>run(async()=>{
    const result=await mutate<{package:InboxPackage}>('package-put',{...(current?{package_id:current.package.id,expected_revision:current.package.revision}:{}),untrusted_name:name.trim(),default_dest_dir:dest.trim()||null})
    setPreview(null);setSelected([]);await refresh(result.package.id)
  })
  const add=()=>run(async()=>{
    if(!current)return
    const links=text.split(/\r?\n/).map(s=>s.trim()).filter(Boolean)
    if(!links.length||links.length>200)throw new Error('Paste 1 to 200 HTTP or HTTPS addresses, one per line.')
    for(const link of links){const url=new URL(link);if(!['http:','https:'].includes(url.protocol))throw new Error('Only HTTP and HTTPS addresses can be added.')}
    const result=await mutate<{added_ids:string[];duplicate_ids:string[]}>('entries-add',{package_id:current.package.id,expected_revision:current.package.revision,entries:links.map(url=>({job:{url},untrusted_label:null}))})
    setText('');setPreview(null);await refresh();setMessage(`${result.added_ids.length} added · ${result.duplicate_ids.length} exact duplicates retained once.`)
  })
  const review=()=>run(async()=>{if(current){const params={package_id:current.package.id,expected_revision:current.package.revision,entry_ids:selected};const reviewed=await mutate<InboxPreview>('preview',params);keys.current.delete(JSON.stringify(['preview',params]));setPreview(reviewed);setReceipt(null)}})
  const start=()=>run(async()=>{if(preview){setReceipt(await mutate<InboxSubmission>('submit',{package_id:preview.package_id,expected_revision:preview.package_revision,preview_id:preview.preview_id}));setPreview(null);setSelected([]);await refresh()}})
  const recover=()=>run(async()=>{if(current?.package.active_batch){setReceipt(await mutate<InboxSubmission>('submission-retry',{package_id:current.package.id,batch_id:current.package.active_batch}));setPreview(null);await refresh()}})
  const remove=()=>run(async()=>{if(current&&selected.length){await mutate('entries-remove',{package_id:current.package.id,expected_revision:current.package.revision,entry_ids:selected});setSelected([]);setPreview(null);await refresh()}})
  const archive=()=>run(async()=>{if(current){await mutate('package-archive',{package_id:current.package.id,expected_revision:current.package.revision,archived:!current.package.archived});setPreview(null);await refresh()}})
  const locked=busy||!!current?.package.active_batch||!!current?.package.archived
  return <div class="inbox-page"><PageHead title="Inbox" sub="Collect links in durable packages, then review destinations before starting downloads." />
    {!NATIVE?<Notice>This inbox requires the desktop engine. Connect the native backend in Settings.</Notice>:<>
      {error?<Notice kind="error">{error}<button class="btn small" onClick={()=>run(()=>refresh())}>Reload receipts</button></Notice>:null}
      {message?<p role="status">{message}</p>:null}
      <div class="inbox-layout"><aside class="inbox-packages" aria-label="Download packages">
        <div class="inbox-actions"><button class="btn small" disabled={busy} onClick={()=>run(()=>refresh())}>Refresh</button><button class="btn small" disabled={busy} onClick={()=>{setCurrent(null);setName('');setDest('');setSelected([]);setPreview(null);setReceipt(null);keys.current.clear()}}>New package</button></div>
        <label class="small"><input type="checkbox" checked={archived} onChange={e=>setArchived(e.currentTarget.checked)} /> Show archived</label>
        {packages.map(pkg=><button key={pkg.id} class={`inbox-package ${current?.package.id===pkg.id?'selected':''}`} disabled={busy} onClick={()=>run(async()=>{setPreview(null);setReceipt(null);setSelected([]);await refresh(pkg.id)})}><strong>{pkg.untrusted_name}</strong><span>{pkg.pending_entries} pending · {pkg.enqueued_entries} started{pkg.archived?' · archived':''}</span>{pkg.active_batch?<span>Dispatch needs reconciliation</span>:null}</button>)}
        {cursor?<button class="btn small" disabled={busy} onClick={()=>run(()=>refresh(undefined,true))}>More packages</button>:null}
      </aside><section class="inbox-content">
        <label class="field">Package name<input aria-label="Package name" value={name} maxLength={160} disabled={locked} onInput={e=>setName(e.currentTarget.value)} /></label>
        <label class="field">Default destination<input aria-label="Default destination" value={dest} disabled={locked} placeholder="An engine download folder; blank uses its default" onInput={e=>setDest(e.currentTarget.value)} /></label>
        <div class="inbox-actions"><button class="btn" disabled={locked||!name.trim()} onClick={save}>{current?'Save package':'Create package'}</button>{current?<button class="btn small" disabled={busy||!!current.package.active_batch} onClick={archive}>{current.package.archived?'Unarchive package':'Archive package'}</button>:null}</div>
        {current?.package.active_batch?<Notice>A saved batch may already have started downloads. Retry this receipt to reconcile its original requests.<button class="btn" disabled={busy} onClick={recover}>Reconcile pending batch</button></Notice>:null}
        {current?<>
          <label class="field">Links, one per line<textarea aria-label="Links, one per line" rows={5} value={text} disabled={locked} onInput={e=>setText(e.currentTarget.value)} /></label>
          <div class="inbox-actions"><button class="btn" disabled={locked||!text.trim()} onClick={add}>Add links to package</button><span class="muted small">Exact requests are stored once. Adding links starts no downloads.</span></div>
          <div class="inbox-actions"><button class="btn primary" disabled={locked||!current.package.pending_entries||selected.length>50} onClick={review}>{selected.length?`Review ${selected.length} selected`:'Review next 50'}</button><button class="btn small" disabled={locked||!selected.length} onClick={remove}>Remove selected from inbox</button></div>
          <div class="inbox-entries" aria-label="Package entries">{current.entries.items.map(entry=><label key={entry.id} class="inbox-entry"><input type="checkbox" checked={selected.includes(entry.id)} disabled={locked||!!entry.job_id} onChange={e=>setSelected(ids=>e.currentTarget.checked?[...ids,entry.id]:ids.filter(id=>id!==entry.id))} /><span><strong>{entry.untrusted_filename||entry.untrusted_label||entry.redacted_url}</strong><span class="mono muted">{entry.redacted_url}</span>{entry.job_id?<a href={href('downloads',{job:entry.job_id})}>{entry.job_id}</a>:<span class="muted">Pending{entry.error?` · ${entry.error.code}`:''}</span>}</span></label>)}</div>
          {current.entries.next_cursor?<button class="btn" disabled={busy} onClick={()=>run(async()=>{const next=await inboxApi.get(current.package.id,current.entries.next_cursor!);setCurrent({...next,entries:{...next.entries,items:[...current.entries.items,...next.entries.items]}})})}>Load next 100 entries</button>:null}
        </>:null}
        {preview?<section class="inbox-review" aria-label="Reviewed download batch"><h3>Review {preview.entries.length} requests</h3><p>{preview.ready_entries} ready · {preview.remaining_pending} pending outside this preview. Review expires {new Date(preview.expires_at).toLocaleTimeString()}.</p>{preview.entries.map(entry=><div key={entry.entry_id} class="inbox-review-row"><span class="mono">{entry.redacted_url}</span><strong class="mono">{entry.dest_path||'No valid destination'}</strong><span>{entry.error?`${entry.error.code} · ${entry.error.message}`:entry.approval_needed?'Waits for a separate session approval':'Public download'}{entry.warnings.length?` · ${entry.warnings.join(', ')}`:''}</span></div>)}<button class="btn primary" disabled={busy||!preview.ready_entries} onClick={start}>Start {preview.ready_entries} reviewed downloads</button></section>:null}
        {receipt?<section aria-label="Download batch receipt"><h3>Saved batch receipt</h3><p class="mono">{receipt.batch_id}</p>{receipt.entries.map(entry=><p key={entry.entry_id}>{entry.result.job_id?<a href={href('downloads',{job:entry.result.job_id})}>{entry.result.job_id}</a>:entry.result.error?.code||'Not accepted'}{entry.result.approval_id?' · awaiting separate approval':''}</p>)}<p>{receipt.remaining_pending} requests remain pending.</p></section>:null}
      </section></div>
    </>}
  </div>
}

import { useEffect, useState } from 'preact/hooks'
import { filesApi, type EverythingConfig, type EverythingPage, type IndexRow } from '../../backend/files'
import { baseOf, parentOf } from '../../lib/paths'
import { fmtBytes } from '../../model/units'
import { Notice } from '../../ui/components'

export function EverythingIntegration({roots,onBrowse}:{roots:string[];onBrowse:(path:string)=>void}) {
  const [config,setConfig]=useState<EverythingConfig|null>(null)
  const [helper,setHelper]=useState('')
  const [instance,setInstance]=useState('1.5a')
  const [pin,setPin]=useState<{helper:string;sha256:string;bytes:number}|null>(null)
  const [root,setRoot]=useState(roots[0]??'')
  const [text,setText]=useState('')
  const [sort,setSort]=useState('name')
  const [descending,setDescending]=useState(false)
  const [page,setPage]=useState<EverythingPage|null>(null)
  const [rows,setRows]=useState<IndexRow[]>([])
  const [error,setError]=useState('')
  const [busy,setBusy]=useState(false)
  const [settings,setSettings]=useState(false)
  useEffect(()=>{let alive=true;void filesApi.everythingConfig().then(c=>{if(!alive)return;setConfig(c);setHelper(c.settings.helper||c.suggested_helper||'');setInstance(c.settings.instance||'1.5a')},e=>{if(alive)setError(e instanceof Error?e.message:'The provider did not answer.')});return()=>{alive=false}},[])
  const inspect=async()=>{setBusy(true);setError('');try{setPin(await filesApi.everythingInspect(helper))}catch(e){setError(e instanceof Error?e.message:'Helper inspection failed.')}finally{setBusy(false)}}
  const enable=async(enabled:boolean)=>{
    if(!config||enabled&&!pin)return
    setBusy(true);setError('')
    try{const next=await filesApi.everythingConfigure({enabled,helper:enabled?pin!.helper:config.settings.helper,sha256:enabled?pin!.sha256:config.settings.sha256,instance,file_id:null});setConfig(next);setPin(null);setSettings(false)}
    catch(e){setError(e instanceof Error?e.message:'The provider settings could not be saved.')}
    finally{setBusy(false)}
  }
  const search=async(offset=0)=>{
    setBusy(true);setError('')
    try{const next=await filesApi.everythingSearch(root,text,sort,descending,offset);setRows(previous=>{const map=new Map((offset?previous:[]).map(r=>[r.path,r]));for(const row of next.files)map.set(row.path,row);return [...map.values()]});setPage(next)}
    catch(e){setError(e instanceof Error?e.message:'The Everything query failed.')}
    finally{setBusy(false)}
  }
  return <section aria-label="Optional Everything integration">
    <p>Search the running Everything index in folders you included in Zeus. Results use live paging and current filesystem metadata. Zeus’s own index remains available in the main search.</p>
    {!config?.supported?<p class="muted">The optional Everything provider is available on Windows.</p>:null}
    {error?<Notice kind="error">{error}</Notice>:null}
    <div class="file-insight-tabs"><span>{config?.settings.enabled?'Everything SDK2 enabled':'Everything SDK2 disabled'}</span><button class="btn small" onClick={()=>setSettings(v=>!v)}>{settings?'Hide helper settings':'Helper settings'}</button>{config?.settings.enabled?<button class="btn small" disabled={busy} onClick={()=>void enable(false)}>Disable provider</button>:null}</div>
    {settings?<>
      <p class="muted small">Use the Zeus read-only Everything broker supplied with this build. Review its path and SHA-256 before enabling it. The helper connects to the selected local Everything instance; file-write permissions remain those you set in Zeus.</p>
      <label class="field">Broker executable path<input value={helper} disabled={busy} placeholder="D:\Zeus\zeus-everything-query.exe" onInput={e=>{setHelper(e.currentTarget.value);setPin(null)}} /></label>
      <label class="field">Everything instance<input value={instance} disabled={busy} placeholder="1.5a" onInput={e=>setInstance(e.currentTarget.value)} /></label>
      <button class="btn small" disabled={busy||!config?.supported||!helper.trim()} onClick={()=>void inspect()}>Inspect selected executable</button>
      {pin?<div class="file-duplicate-candidate"><p class="mono">{pin.helper}</p><p>{fmtBytes(pin.bytes)} · SHA-256 <span class="mono">{pin.sha256}</span></p><button class="btn primary" disabled={busy} onClick={()=>void enable(true)}>Enable reviewed broker</button></div>:null}
    </>:null}
    <label class="field">Included folder<select aria-label="Included folder" value={root} disabled={busy} onChange={e=>{setRoot(e.currentTarget.value);setRows([]);setPage(null)}}>{roots.map(path=><option value={path}>{path}</option>)}</select></label>
    <label class="field">Literal filename text<input value={text} disabled={busy} onInput={e=>{setText(e.currentTarget.value);setRows([]);setPage(null)}} onKeyDown={e=>{if(e.key==='Enter'&&config?.settings.enabled)void search()}} /></label>
    <div class="file-insight-tabs"><select aria-label="Everything result order" value={sort} onChange={e=>{setSort(e.currentTarget.value);setRows([]);setPage(null)}}><option value="name">Name</option><option value="path">Path</option><option value="size">Size</option><option value="modified">Modified</option></select><label><input type="checkbox" checked={descending} onChange={e=>{setDescending(e.currentTarget.checked);setRows([]);setPage(null)}} /> Descending</label><button class="btn primary" disabled={busy||!config?.settings.enabled||!root} onClick={()=>void search()}>Search Everything</button></div>
    {page?<p role="status">{rows.length.toLocaleString()} results loaded · {page.elapsed_ms}ms · {page.skipped} stale, excluded or out-of-scope rows skipped. Refresh starts a new live query.</p>:null}
    <div class="everything-results">{rows.map(row=><div class="file-insight-row"><span class="mono">{baseOf(row.path)}<br /><span class="muted">{parentOf(row.path)}</span></span><span>{row.is_dir?'Folder':fmtBytes(row.size)}</span><button class="btn small" onClick={()=>onBrowse(row.is_dir?row.path:parentOf(row.path))}>Browse folder</button></div>)}</div>
    {page?.next!=null?<button class="btn" disabled={busy} onClick={()=>void search(page.next!)}>Load next page</button>:null}
  </section>
}

import { useEffect, useRef, useState } from 'preact/hooks'
import { NATIVE } from '../backend/live'
import { workflowApi as api, workflowTemplate, type DryRun, type Finding, type PlanReview, type RunPage, type SavedPlan, type WorkflowRun } from '../backend/workflows'
import { fmtBytes } from '../model/units'
import { Notice, PageHead, StateLabel } from '../ui/components'
import '../styles/runs.css'

const key = () => `desktop-${crypto.randomUUID()}`
const message = (e: unknown) => e instanceof Error ? e.message : 'The engine could not complete the request.'
const lines = (text: string) => text.split(/\r?\n/).map(s => s.trim()).filter(Boolean)
const findings = (rows: Finding[]) => rows.length ? <ul>{rows.map((row, i) => <li key={i}><strong>{row.severity}: {row.code}</strong> {row.message}</li>)}</ul> : null

export function Runs() {
  const [goal, setGoal] = useState('Download linked files')
  const [urls, setUrls] = useState('')
  const [dest, setDest] = useState('workflow-downloads')
  const [text, setText] = useState('')
  const [saved, setSaved] = useState<SavedPlan[]>([])
  const [savedError, setSavedError] = useState('')
  const [savedReady, setSavedReady] = useState(false)
  const [review, setReview] = useState<PlanReview | null>(null)
  const [dry, setDry] = useState<DryRun | null>(null)
  const [busy, setBusy] = useState(false)
  const working = useRef(false)
  const mounted = useRef(true)
  const draft = useRef<{ id: string; urls: string } | null>(null)
  const mutation = useRef<{ signature: string; key: string } | null>(null)
  const [error, setError] = useState('')
  const [note, setNote] = useState('')
  const [page, setPage] = useState<RunPage>({ items: [], next_cursor: null })
  const [cursors, setCursors] = useState<string[]>([])
  const cursor = cursors.at(-1)
  const [readError, setReadError] = useState('')
  const [loading, setLoading] = useState(true)
  const [refresh, setRefresh] = useState(0)
  const [selected, setSelected] = useState('')
  const [report, setReport] = useState<WorkflowRun | null>(null)
  const [reportError, setReportError] = useState('')
  const [manifest, setManifest] = useState<{ path: string; records: number; content?: string; sha256?: string } | null>(null)

  useEffect(() => {
    mounted.current = true
    if (NATIVE) void api.saved().then(value => {
      if (mounted.current) { setSaved(value); setSavedReady(true) }
    }).catch(e => { if (mounted.current) setSavedError(message(e)) })
    return () => { mounted.current = false }
  }, [])
  useEffect(() => {
    if (!NATIVE) return
    let live = true, timer: ReturnType<typeof setTimeout> | undefined
    setLoading(true)
    const tick = async () => {
      try { const value = await api.list(cursor); if (live) { setPage(value); setReadError('') } }
      catch (e) { if (live) setReadError(message(e)) }
      finally { if (live) { setLoading(false); timer = setTimeout(() => void tick(), 4000) } }
    }
    void tick()
    return () => { live = false; clearTimeout(timer) }
  }, [cursor, refresh])
  useEffect(() => {
    setReport(null); setManifest(null); setReportError('')
    if (!selected || !NATIVE) return
    let live = true, timer: ReturnType<typeof setTimeout> | undefined
    const tick = async () => {
      try { const value = await api.get(selected); if (live) { setReport(value); setReportError('') } }
      catch (e) { if (live) setReportError(message(e)) }
      finally { if (live) timer = setTimeout(() => void tick(), 3000) }
    }
    void tick()
    return () => { live = false; clearTimeout(timer) }
  }, [selected, refresh])
  const action = async (f: () => Promise<void>) => {
    if (working.current) return
    working.current = true; setBusy(true); setError(''); setNote('')
    try { await f() } catch (e) { if (mounted.current) setError(message(e)) }
    finally { working.current = false; if (mounted.current) setBusy(false) }
  }
  const retry = (signature: string) => {
    if (mutation.current?.signature !== signature) mutation.current = { signature, key: key() }
    return mutation.current.key
  }
  const changed = () => { setReview(null); setDry(null); mutation.current = null; setNote('') }
  const generate = () => void action(async () => {
    const candidate = workflowTemplate(lines(urls), goal, dest)
    changed(); setText(JSON.stringify(candidate, null, 2))
  })
  const validate = () => void action(async () => {
    const candidate: unknown = JSON.parse(text)
    if (!candidate || Array.isArray(candidate) || typeof candidate !== 'object') throw new Error('The plan must be a JSON object.')
    const start_urls = lines(urls)
    if (!start_urls.length || start_urls.length > 3) throw new Error('Enter one to three page URLs to sample.')
    // Reuse an existing plan only with the exact page set that supplied its samples.
    const params = { goal, start_urls, candidate, ...(draft.current?.urls === urls ? { plan_id: draft.current.id } : {}) }
    setReview(null); setDry(null)
    const value = await api.plan({ ...params, idempotency_key: retry(`plan:${JSON.stringify(params)}`) })
    if (!mounted.current) return
    draft.current = { id: value.plan_id, urls }; mutation.current = null
    setReview(value)
    if (value.plan && value.plan_hash) setText(JSON.stringify(value.plan, null, 2))
  })
  const sample = () => void action(async () => {
    if (!review?.plan_hash) return
    const value = await api.dry(review.plan_id)
    if (!mounted.current) return
    if (value.plan_hash !== review.plan_hash) throw new Error('The plan changed. Validate it again before continuing.')
    setDry(value)
  })
  const save = () => void action(async () => {
    if (!review?.plan || !review.plan_hash || !savedReady) return
    const row: SavedPlan = { goal, urls, text: JSON.stringify(review.plan, null, 2), plan_id: review.plan_id, plan_hash: review.plan_hash }
    const next = [row, ...saved.filter(p => p.plan_id !== row.plan_id)].slice(0, 30)
    await api.save(next)
    if (mounted.current) { setSaved(next); setNote('Exact validated plan saved on this computer. Reopening it requires a fresh validation and dry run.') }
  })
  const requestRun = () => void action(async () => {
    if (!review?.plan_hash || !dry?.complete || dry.plan_hash !== review.plan_hash) return
    const value = await api.run(review.plan_id, review.plan_hash, retry(`run:${review.plan_id}:${review.plan_hash}`))
    if (!mounted.current) return
    mutation.current = null; setDry(null); setSelected(value.run_id); setCursors([]); setRefresh(n => n + 1)
    setNote('Run created. Open its approval review to inspect and confirm the exact plan in the Zeus terminal.')
  })
  const control = (command: 'pause' | 'resume' | 'cancel') => void action(async () => {
    if (!report) return
    const result = await api.control(report.run_id, command, retry(`control:${report.run_id}:${command}`))
    if (mounted.current) { mutation.current = null; setReport(result.run); setRefresh(n => n + 1) }
  })
  const exportManifest = (format: 'csv' | 'jsonl') => void action(async () => {
    if (!report) return
    const value = await api.manifest(report.run_id, format, retry(`manifest:${report.run_id}:${format}`))
    if (mounted.current) { mutation.current = null; setManifest(value) }
  })
  if (!NATIVE) return <div class="page-body"><PageHead title="Workflow runs" sub="Plan, review and monitor bounded downloads from public pages." /><Notice>Open Zeus desktop to manage workflow runs on this computer.</Notice></div>
  return <div class="page-body runs-page">
    <PageHead title="Workflow runs" sub="Validate a plan, inspect its dry run, then request approval before downloads begin." />
    {error && <Notice kind="error">{error}</Notice>}{note && <Notice>{note}</Notice>}
    <section class="runs-card" aria-label="Workflow plan">
      <h2>Plan a workflow</h2>
      <p class="muted">Start with public pages containing download links. Edit the JSON for selectors, pagination, naming and limits. Session pages, infinite scroll and workflow conversion are not available in this release.</p>
      {savedError && <Notice kind="error">Saved plans could not be read: {savedError}. Saving is disabled to protect the existing list.</Notice>}
      {!!saved.length && <label class="field">Saved plans<select aria-label="Saved plans" value="" disabled={busy} onChange={e => {
        const p = saved.find(row => row.plan_id === e.currentTarget.value)
        if (!p) return
        changed(); setGoal(p.goal); setUrls(p.urls); setText(p.text); draft.current = { id: p.plan_id, urls: p.urls }
      }}><option value="">Open a saved plan…</option>{saved.map(p => <option key={p.plan_id} value={p.plan_id}>{p.goal} · {p.plan_id}</option>)}</select></label>}
      <div class="runs-grid">
        <label class="field">Goal<input aria-label="Workflow goal" value={goal} maxLength={2000} disabled={busy} onInput={e => { setGoal(e.currentTarget.value); changed() }} /></label>
        <label class="field">Destination folder<input aria-label="Workflow destination" value={dest} disabled={busy} onInput={e => setDest(e.currentTarget.value)} /><small class="muted">Used when creating the starting plan; the JSON below controls execution.</small></label>
      </div>
      <label class="field">Public page URLs · one per line<textarea aria-label="Workflow page URLs" value={urls} spellcheck={false} maxLength={24000} disabled={busy} onInput={e => { setUrls(e.currentTarget.value); changed() }} /></label>
      <div class="row-actions"><button class="btn" disabled={busy} onClick={generate}>Create starting plan</button><span class="muted">Matches links with a download attribute; review and edit the selector for your pages.</span></div>
      <label class="field">Plan JSON<textarea class="runs-plan" aria-label="Plan JSON" value={text} spellcheck={false} maxLength={262144} disabled={busy} onInput={e => { setText(e.currentTarget.value); changed() }} /></label>
      <div class="row-actions">
        <button class="btn primary" disabled={busy || !text} onClick={validate}>Validate and sample pages</button>
        <button class="btn" disabled={busy || !review?.plan_hash} onClick={sample}>Dry run</button>
        <button class="btn" disabled={busy || !review?.plan_hash || !savedReady} onClick={save}>Save exact plan</button>
        <button class="btn primary" disabled={busy || !review?.plan_hash || !dry?.complete || dry.plan_hash !== review.plan_hash} onClick={requestRun}>Request run approval</button>
      </div>
      {busy && <p role="status">Waiting for the engine…</p>}
      {review && <div aria-label="Plan validation"><p>{review.plan_hash ? 'Validated plan' : 'Plan needs changes'} · <code>{review.plan_id}</code></p>{review.plan_hash && <p>Plan hash: <code>{review.plan_hash}</code></p>}{findings(review.diagnostics)}<p>{review.samples.length} page samples inspected.</p></div>}
      {dry && <div aria-label="Dry run report"><h3>Dry run</h3><p>{dry.complete ? 'Sampling complete' : 'Sampling incomplete; run approval is disabled'} · {dry.sampled_pages} pages · {dry.name_collisions} naming collisions</p>{findings(dry.warnings)}<div class="runs-scroll"><table><thead><tr><th>Source</th><th>File name</th><th>Collision</th></tr></thead><tbody>{dry.sample.map((item, i) => <tr key={i}><td>{item.url_redacted}</td><td>{item.file_name}</td><td>{item.collides_with === undefined || item.collides_with === null ? '—' : `Item ${item.collides_with + 1}`}</td></tr>)}</tbody></table></div></div>}
    </section>
    <section class="runs-card" aria-label="Saved workflow runs">
      <div class="row-actions"><h2>Runs</h2><button class="btn" disabled={busy || loading} onClick={() => { setCursors([]); setRefresh(n => n + 1) }}>Refresh runs</button></div>
      {readError && <Notice kind="error">{readError} {page.items.length ? 'The last loaded list remains below.' : ''}</Notice>}
      <div class="runs-scroll" aria-busy={loading}><table><thead><tr><th>Run</th><th>Status</th><th>Pages</th><th>Files</th><th>Bytes</th></tr></thead><tbody>{page.items.map(row => <tr key={row.run_id}><td><button class="btn run-id" aria-pressed={selected === row.run_id} onClick={() => setSelected(row.run_id)}>{row.run_id}</button></td><td><StateLabel state={row.status} /></td><td>{row.pages_fetched}</td><td>{row.files_completed}/{row.files_enqueued}</td><td>{fmtBytes(row.bytes_done)}</td></tr>)}</tbody></table></div>
      {!loading && !readError && !page.items.length && <p>No workflow runs on this page.</p>}
      <div class="row-actions"><button class="btn" disabled={loading || !cursors.length} onClick={() => setCursors(c => c.slice(0, -1))}>Previous page</button><span>Page {cursors.length + 1} · up to 50 runs</span><button class="btn" disabled={loading || !page.next_cursor} onClick={() => { if (page.next_cursor) setCursors(c => [...c, page.next_cursor!]) }}>Next page</button></div>
    </section>
    {selected && <section class="runs-card" aria-label="Workflow run details">
      <h2>Run details</h2><code>{selected}</code>{reportError && <Notice kind="error">{reportError}</Notice>}
      {report && <><p><StateLabel state={report.status} /> · {fmtBytes(report.bytes_done)} · {Math.round(report.elapsed_ms / 1000)} seconds active</p><p>{report.pages_fetched} pages · {report.files_completed}/{report.files_enqueued} files complete · {report.files_failed} failed</p><p>Plan hash: <code>{report.plan_hash}</code></p>
        {report.possibly_incomplete && <Notice kind="warn">This run may not have explored all sources. {report.pause_reason ?? report.stop_reason ?? ''}</Notice>}
        {report.last_error && <Notice kind="error">{report.last_error.code}: {report.last_error.message}</Notice>}{findings(report.drift)}
        <div class="row-actions">
          {report.status === 'awaiting_approval' && report.approval_id && <button class="btn primary" disabled={busy} onClick={() => void action(async () => { await api.review(report.approval_id!); if (mounted.current) setNote('Review the plan and make your decision in the opened terminal.') })}>Open approval review</button>}
          <button class="btn" disabled={busy || report.status !== 'running'} onClick={() => control('pause')}>Pause run</button>
          <button class="btn" disabled={busy || report.status !== 'paused'} onClick={() => control('resume')}>Resume run</button>
          <button class="btn" disabled={busy || !['running', 'paused', 'awaiting_approval'].includes(report.status)} onClick={() => control('cancel')}>Cancel run</button>
          <button class="btn" disabled={busy} onClick={() => exportManifest('csv')}>Export CSV manifest</button><button class="btn" disabled={busy} onClick={() => exportManifest('jsonl')}>Export JSONL manifest</button>
        </div>
        {report.counts && <details><summary>Detailed counters</summary><pre>{JSON.stringify(report.counts, null, 2)}</pre></details>}
      </>}
      {manifest && <div aria-label="Exported workflow manifest"><p>{manifest.records} records saved to <code>{manifest.path}</code></p>{manifest.sha256 && <p>SHA-256: <code>{manifest.sha256}</code></p>}{manifest.content && <details><summary>Manifest contents</summary><pre>{manifest.content}</pre></details>}</div>}
    </section>}
  </div>
}

import { useEffect, useRef, useState } from 'preact/hooks'
import { libraryApi as api, type ArtifactEvents, type MaintenanceOperation, type MaintenanceReceipt, type MaintenanceRequest } from '../backend/library'
import { Notice } from '../ui/components'
import { fmtBytes } from '../model/units'

const json = (value: unknown) => JSON.stringify(value, null, 2)
const message = (error: unknown) => error instanceof Error ? error.message : 'Artifact maintenance failed.'
const names = { export: 'Export bundle', import: 'Import bundle', collect: 'Collect unprotected content', repair: 'Repair stored content', scrub: 'Check stored content integrity' }
type Kind = keyof typeof names

/** Restore only a saved request. The daemon revalidates every plan and never trusts this file as authority. */
function restore(text: string): MaintenanceRequest {
  if (new TextEncoder().encode(text).length > 262144) throw new Error('Saved reviews are limited to 256 KiB.')
  const value: unknown = JSON.parse(text)
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Choose a saved maintenance request.')
  const request = value as Record<string, unknown>
  if (Object.keys(request).some(key => !['plan', 'plan_hash', 'idempotency_key'].includes(key)) ||
    typeof request.plan_hash !== 'string' || !/^[0-9a-f]{64}$/.test(request.plan_hash) ||
    typeof request.idempotency_key !== 'string' || !request.idempotency_key.length || request.idempotency_key.length > 128 ||
    /[\u0000-\u001f\u007f]/.test(request.idempotency_key) || !request.plan || typeof request.plan !== 'object') {
    throw new Error('Use the exact saved plan, hash and retry key.')
  }
  const plan = request.plan as Record<string, unknown>
  const operation = plan.operation as Record<string, unknown> | undefined
  if (!operation || typeof operation.kind !== 'string' || !Object.hasOwn(names, operation.kind) ||
    typeof plan.requires_approval !== 'boolean' || typeof plan.expires_at !== 'string' || !Number.isFinite(Date.parse(plan.expires_at))) {
    throw new Error('The saved maintenance plan is incomplete.')
  }
  return request as MaintenanceRequest
}

export function ArtifactMaintenance({ selected, onChanged }: { selected: string; onChanged: () => void }) {
  const [kind, setKind] = useState<Kind>('export'), [ids, setIds] = useState(''), [path, setPath] = useState('')
  const [include, setInclude] = useState(false), [pressure, setPressure] = useState(false), [limit, setLimit] = useState('16')
  const [request, setRequest] = useState<MaintenanceRequest | null>(null), [receipt, setReceipt] = useState<MaintenanceReceipt | null>(null)
  const [events, setEvents] = useState<ArtifactEvents | null>(null), [eventError, setEventError] = useState('')
  const [busy, setBusy] = useState(false), [error, setError] = useState(''), [statusError, setStatusError] = useState('')
  const gate = useRef(false), alive = useRef(true), epoch = useRef(0)
  useEffect(() => { alive.current = true; return () => { alive.current = false } }, [])
  const run = (fn: () => Promise<void>) => {
    if (gate.current) return
    gate.current = true; setBusy(true); setError('')
    void fn().catch(e => { if (alive.current) setError(message(e)) }).finally(() => { gate.current = false; if (alive.current) setBusy(false) })
  }
  const change = () => { epoch.current++; setRequest(null); setReceipt(null); setStatusError('') }
  const refreshStatus = async (exact: MaintenanceRequest) => {
    const generation = epoch.current
    try { const result = await api.maintenanceStatus(exact); if (alive.current && epoch.current === generation) { setReceipt(result); setStatusError('') } }
    catch (e) { if (alive.current && epoch.current === generation) setStatusError(message(e)) }
  }
  // Poll receipts only. Approval and retry always require another explicit click.
  useEffect(() => {
    if (!request || !receipt || receipt.applied || !['in_progress', 'awaiting_approval'].includes(receipt.status)) return
    const timer = setInterval(() => { if (!gate.current) void refreshStatus(request) }, 3000)
    return () => clearInterval(timer)
  }, [request, receipt?.status, receipt?.applied])
  const preview = () => run(async () => {
    const artifact_ids = ids.split(/[\s,]+/).filter(Boolean)
    if (['export', 'repair', 'scrub'].includes(kind) && (!artifact_ids.length || artifact_ids.length > 64 || new Set(artifact_ids).size !== artifact_ids.length)) throw new Error('Enter 1–64 different artifact IDs.')
    if (['export', 'import', 'repair'].includes(kind) && !path.trim()) throw new Error('Enter a file path inside a managed download folder.')
    let operation: MaintenanceOperation
    if (kind === 'export') operation = { kind, artifact_ids, path: path.trim(), include_objects: include }
    else if (kind === 'import') operation = { kind, path: path.trim() }
    else if (kind === 'repair') {
      if (artifact_ids.length !== 1) throw new Error('Repair uses exactly one artifact ID.')
      operation = { kind, artifact_id: artifact_ids[0]!, path: path.trim() }
    } else if (kind === 'scrub') operation = { kind, artifact_ids }
    else {
      const count = Number(limit)
      if (!Number.isInteger(count) || count < 1 || count > 128) throw new Error('Choose 1–128 collection targets. Large exact plans may need a smaller batch.')
      operation = { kind, quota_pressure: pressure, limit: count }
    }
    change()
    const value = await api.maintenancePreview(operation)
    if (alive.current) setRequest({ ...value, idempotency_key: `maintenance-${crypto.randomUUID()}` })
  })
  const apply = () => run(async () => {
    if (!request) return
    epoch.current++
    const result = await api.maintenanceApply(request)
    if (alive.current) { setReceipt(result); setStatusError(''); if (result.applied) onChanged() }
  })
  const save = () => {
    if (!request) return
    const url = URL.createObjectURL(new Blob([json(request)], { type: 'application/json' }))
    const link = document.createElement('a'); link.href = url; link.download = `zeus-maintenance-${request.plan_hash.slice(0, 12)}.json`; link.click()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  }
  const readEvents = (more: boolean) => run(async () => {
    try { const value = await api.events(more ? events?.next_seq : 0, more ? events?.head.chain_id : undefined); if (alive.current) { setEvents(value); setEventError('') } }
    catch (e) { if (alive.current) setEventError(message(e)) }
  })
  const expired = request ? Date.parse(request.plan.expires_at) <= Date.now() : false
  return <section class="runs-card" aria-label="Artifact maintenance"><h2>Artifact maintenance</h2>
    <p>Move bounded bundles between stores, verify content and review storage cleanup. Every operation starts with an exact preview.</p>
    {error && <Notice kind="error">{error}</Notice>}
    <div class="runs-grid"><label class="field">Operation<select aria-label="Maintenance operation" value={kind} disabled={busy} onChange={e => { change(); setKind(e.currentTarget.value as Kind) }}>{Object.entries(names).map(([value, label]) => <option value={value}>{label}</option>)}</select></label>
      <label class="field">Restore a saved request<input aria-label="Restore maintenance request" type="file" accept=".json,application/json" disabled={busy} onChange={e => { const file = e.currentTarget.files?.[0]; e.currentTarget.value = ''; if (file) run(async () => { if (file.size > 262144) throw new Error('Saved reviews are limited to 256 KiB.'); const exact = restore(await file.text()); change(); setRequest(exact); await refreshStatus(exact) }) }} /></label></div>
    {['export', 'repair', 'scrub'].includes(kind) && <><label class="field">Artifact IDs · separated by lines or commas<textarea aria-label="Maintenance artifact IDs" value={ids} disabled={busy} maxLength={4096} onInput={e => { change(); setIds(e.currentTarget.value) }} /></label><button class="btn" disabled={busy || !selected} onClick={() => { change(); setIds(selected) }}>Use selected artifact</button></>}
    {['export', 'import', 'repair'].includes(kind) && <label class="field">{kind === 'export' ? 'New bundle destination · .zeusab' : kind === 'repair' ? 'Verified repair source file' : 'Existing bundle file'}<input aria-label="Maintenance file path" value={path} disabled={busy} maxLength={4096} onInput={e => { change(); setPath(e.currentTarget.value) }} /></label>}
    {kind === 'export' && <><label class="row-actions"><input style={{ width: 'auto' }} type="checkbox" checked={include} disabled={busy} onChange={e => { change(); setInclude(e.currentTarget.checked) }} />Include verified stored bytes</label><p class="muted">Metadata-only export preserves records and provenance. Bundles support up to 64 records, 16 MiB per object and 32 MiB total. Existing destination files are never replaced; quarantined bytes cannot be exported.</p></>}
    {kind === 'import' && <p class="muted">The whole bundle is validated before importing. Existing immutable content and records are never overwritten. Partial imports can resume with the same saved request.</p>}
    {kind === 'collect' && <><div class="runs-grid"><label class="field">Maximum objects<input aria-label="Collection target limit" type="number" min="1" max="128" value={limit} disabled={busy} onInput={e => { change(); setLimit(e.currentTarget.value) }} /></label><label class="row-actions"><input style={{ width: 'auto' }} type="checkbox" checked={pressure} disabled={busy} onChange={e => { change(); setPressure(e.currentTarget.checked) }} />Include active evictable content</label></div><Notice>Collection permanently removes the exact unprotected stored copies in the approved plan. Downloaded user files remain. Current ownership, retention and object identities are checked again before removal.</Notice></>}
    {kind === 'repair' && <p class="muted">Repair verifies both content hashes against one immutable record before restoring its stored bytes. It requires human approval and does not clear quarantine.</p>}
    {kind === 'scrub' && <p class="muted">After human approval, verified bytes remain available. Mismatched stored bytes move to private damaged storage for recovery; they are not silently replaced.</p>}
    <button class="btn" disabled={busy} onClick={preview}>Preview maintenance</button>
    {request && <div class="runs-card" aria-label="Exact maintenance review"><h3>{names[request.plan.operation.kind]}</h3><p>Review hash: <code>{request.plan_hash}</code></p><p>Expires: {request.plan.expires_at}</p><pre aria-label="Exact maintenance plan">{json(request.plan)}</pre><p class="muted">Save this request before applying to preserve its exact plan and retry key across restarts. Restoring it only reads status.</p>
      <div class="row-actions"><button class="btn" disabled={busy} onClick={save}>Save maintenance request</button><button class="btn" disabled={busy} onClick={() => run(() => refreshStatus(request))}>Read maintenance status</button><button class="btn primary" disabled={busy || expired || !!receipt?.applied || receipt?.status === 'in_progress' || !!receipt?.reconciliation_required} onClick={apply}>{receipt?.approval_id ? 'Check approval and apply exact plan' : request.plan.requires_approval ? 'Request maintenance approval' : 'Apply reviewed maintenance'}</button></div>
      {expired && <Notice>This preview has expired. Status and saved receipts remain readable; create a new preview for new effects.</Notice>}
      {statusError && <Notice kind="error">{statusError}</Notice>}
      {receipt && <div aria-label="Maintenance receipt" aria-live="polite"><p>Status: {receipt.status.replaceAll('_', ' ')}{receipt.replayed ? ' · saved result' : ''}{receipt.size_bytes ? ` · ${fmtBytes(receipt.size_bytes)}` : ''}</p>{receipt.error_code && <Notice kind="error">{receipt.error_code}</Notice>}
        <p>{receipt.artifact_ids.length} imported records · {receipt.verified_objects.length} verified objects · {receipt.damaged_objects.length} damaged objects</p>{receipt.absent_objects !== null && <p>{receipt.absent_objects} exact targets known absent · {receipt.remaining_objects} not yet known absent</p>}
        {receipt.reconciliation_required && <Notice>Inspect the exact target and partial receipt before a fresh preview. An interrupted export may have published its file. Zeus will not adopt or replace that file as a successful retry.</Notice>}
        {receipt.approval_id && !receipt.applied && <><p>Human approval: <code>{receipt.approval_id}</code></p><button class="btn" disabled={busy} onClick={() => run(async () => { await api.review(receipt.approval_id!) })}>Review maintenance in human terminal</button></>}
        <details><summary>Full maintenance receipt</summary><pre>{json(receipt)}</pre></details></div>}
    </div>}
    <details><summary>Artifact event replay</summary><p class="muted">Read retained audit events in order. Intent events describe a planned effect; only completion events confirm its recorded completion. A retention gap requires refreshing the library.</p><div class="row-actions"><button class="btn" disabled={busy} onClick={() => readEvents(false)}>Read artifact events</button><button class="btn" disabled={busy || !events?.has_more} onClick={() => readEvents(true)}>Next artifact events</button></div>{eventError && <Notice kind="error">{eventError}</Notice>}{events && <>{events.history_truncated && <Notice>Earlier audit history is no longer retained. Refresh artifact records before relying on this replay.</Notice>}<p>Store format {events.store_format_version} · next sequence {events.next_seq}</p><pre aria-label="Artifact events">{json(events.events)}</pre></>}</details>
  </section>
}

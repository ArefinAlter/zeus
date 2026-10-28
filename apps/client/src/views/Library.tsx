import { useEffect, useRef, useState } from 'preact/hooks'
import { NATIVE } from '../backend/live'
import { libraryApi as api, type Artifact, type ArtifactPage, type Lifecycle, type LifecycleReceipt, type Lineage, type Organization, type OperationStatus } from '../backend/library'
import { fmtBytes } from '../model/units'
import { Notice, PageHead } from '../ui/components'
import '../styles/runs.css'

const errorText = (e: unknown) => e instanceof Error ? e.message : 'The library request failed.'
const json = (value: unknown) => JSON.stringify(value, null, 2)
const initialRules = json({ recursive: false, rules: [{ when: { ext: ['pdf'] }, subdir: 'Documents/{year}' }] })

export function Library() {
  const [rows, setRows] = useState<ArtifactPage>({ items: [] })
  const [cursors, setCursors] = useState<string[]>([]), cursor = cursors.at(-1)
  const [filter, setFilter] = useState('')
  const [selected, setSelected] = useState('')
  const [record, setRecord] = useState<Artifact | null>(null)
  const [lineage, setLineage] = useState<Lineage | null>(null)
  const [readError, setReadError] = useState(''), [detailError, setDetailError] = useState('')
  const [error, setError] = useState(''), [note, setNote] = useState('')
  const [refresh, setRefresh] = useState(0), [busy, setBusy] = useState(false)
  const gate = useRef(false), alive = useRef(true)
  const retry = useRef<{ signature: string; key: string } | null>(null)
  const [ingest, setIngest] = useState('')
  const [effect, setEffect] = useState('keep'), [path, setPath] = useState(''), [until, setUntil] = useState('')
  const [preview, setPreview] = useState<Lifecycle | null>(null), [receipt, setReceipt] = useState<LifecycleReceipt | null>(null)
  const [dir, setDir] = useState(''), [rules, setRules] = useState(initialRules)
  const [organization, setOrganization] = useState<Organization | null>(null)
  const [reviewedRules, setReviewedRules] = useState<{ dir: string; rules: unknown } | null>(null)
  const [undoId, setUndoId] = useState('')
  const [operationStatus, setOperationStatus] = useState<OperationStatus | null>(null)
  const [operationError, setOperationError] = useState('')
  const operationId = organization?.operation_id
  useEffect(() => {
    setOperationStatus(null); setOperationError('')
    if (!NATIVE || !operationId) return
    let live = true, timer: ReturnType<typeof setTimeout> | undefined
    const tick = async () => {
      try { const result = await api.operation(operationId); if (live) { setOperationStatus(result); setOperationError('') } }
      catch (e) { if (live) setOperationError(errorText(e)) }
      finally { if (live) timer = setTimeout(() => void tick(), 3000) }
    }
    void tick(); return () => { live = false; clearTimeout(timer) }
  }, [operationId])
  useEffect(() => { alive.current = true; return () => { alive.current = false } }, [])
  useEffect(() => {
    if (!NATIVE) return
    let live = true, timer: ReturnType<typeof setTimeout> | undefined
    const tick = async () => {
      try { const value = await api.list(cursor, filter); if (live) { setRows(value); setReadError('') } }
      catch (e) { if (live) setReadError(errorText(e)) }
      finally { if (live) timer = setTimeout(() => void tick(), 4000) }
    }
    void tick(); return () => { live = false; clearTimeout(timer) }
  }, [cursor, filter, refresh])
  useEffect(() => {
    setRecord(null); setLineage(null); setDetailError(''); setPreview(null); setReceipt(null)
    if (!NATIVE || !selected) return
    let live = true
    void api.get(selected).then(value => { if (live) setRecord(value.artifact) }).catch(e => { if (live) setDetailError(errorText(e)) })
    return () => { live = false }
  }, [selected, refresh])
  const run = (fn: () => Promise<void>) => {
    if (gate.current) return
    gate.current = true; setBusy(true); setError(''); setNote('')
    void fn().catch(e => { if (alive.current) setError(errorText(e)) }).finally(() => { gate.current = false; if (alive.current) setBusy(false) })
  }
  const key = (signature: string) => {
    if (retry.current?.signature !== signature) retry.current = { signature, key: `desktop-${crypto.randomUUID()}` }
    return retry.current.key
  }
  const changed = () => { setPreview(null); setReceipt(null); retry.current = null }
  const ruleChanged = () => { setReviewedRules(null); setOrganization(null); retry.current = null }
  const inspect = () => run(async () => {
    if (!record) return
    let operation: unknown
    if (effect === 'delete') operation = { kind: 'delete' }
    else if (effect === 'materialize') {
      if (!path.trim()) throw new Error('Enter a new destination file path.')
      operation = { kind: 'materialize', path: path.trim() }
    } else {
      if (until && !Number.isFinite(Date.parse(until))) throw new Error('Use a valid protection deadline, including its time zone.')
      operation = { kind: 'set_retention', retention: effect, retain_until: until ? new Date(until).toISOString() : null }
    }
    changed()
    const result = await api.preview(record.id, operation)
    if (alive.current) setPreview(result)
  })
  const apply = () => run(async () => {
    if (!preview) return
    const result = await api.apply(preview, key(`artifact:${json(preview)}`))
    if (alive.current) {
      setReceipt(result)
      if (result.applied) { retry.current = null; setPreview(null); if (result.artifact) setRecord(result.artifact); setNote('The reviewed change completed.'); }
    }
  })
  const organizePlan = () => run(async () => {
    ruleChanged()
    if (!dir.trim() || rules.length > 65536) throw new Error('Enter a folder and at most 64 KiB of rules.')
    const parsed: unknown = JSON.parse(rules)
    if (!parsed || Array.isArray(parsed) || typeof parsed !== 'object') throw new Error('Use a deterministic JSON rule set.')
    const exact = { dir: dir.trim(), rules: parsed }
    const value = await api.organize('plan', exact.dir, exact.rules, key(`plan:${json(exact)}`))
    if (alive.current) { setOrganization(value); setReviewedRules(exact); retry.current = null }
  })
  const organizeRequest = () => run(async () => {
    if (!reviewedRules) return
    const value = await api.organize('request', reviewedRules.dir, reviewedRules.rules, key(`organize:${json(reviewedRules)}`))
    if (alive.current) { setOrganization(value); if (value.operation_id) setUndoId(value.operation_id) }
  })
  if (!NATIVE) return <div class="page-body"><PageHead title="Artifact library and rules" /><Notice>Open Zeus desktop to inspect artifacts and organize local files.</Notice></div>
  return <div class="page-body runs-page">
    <PageHead title="Artifact library and rules" sub="Inspect provenance, manage stored content and review deterministic file rules." />
    {error && <Notice kind="error">{error}</Notice>}{note && <Notice>{note}</Notice>}
    <section class="runs-card" aria-label="Artifact library"><h2>Artifacts</h2>
      {readError && <Notice kind="error">{readError} The last loaded page remains visible.</Notice>}
      <div class="row-actions"><label class="field">Record state<select aria-label="Artifact state" disabled={busy} value={filter} onChange={e => { setFilter(e.currentTarget.value); setCursors([]) }}><option value="">All records</option><option value="active">Active</option><option value="deleted">Tombstones</option></select></label><button class="btn" disabled={busy} onClick={() => setRefresh(n => n + 1)}>Refresh library</button></div>
      <p class="muted">Up to 50 records per page. An artifact record can exist without a stored copy of its bytes.</p>
      <div class="runs-scroll"><table><thead><tr><th>Record</th><th>Bytes</th><th>State</th><th>Stored content</th></tr></thead><tbody>{rows.items.map(row => <tr key={row.id}><td><button class="btn" disabled={busy} aria-pressed={selected === row.id} onClick={() => setSelected(row.id)}>{row.name || row.id}</button><div class="run-id">{row.id}</div></td><td>{fmtBytes(row.size_bytes)}</td><td>{row.state}</td><td>{row.object.state}{row.quarantined ? ' · quarantined' : ''}</td></tr>)}</tbody></table></div>
      {!rows.items.length && !readError && <p>No artifact records on this page.</p>}
      <div class="row-actions"><button class="btn" disabled={busy || !cursors.length} onClick={() => setCursors(old => old.slice(0, -1))}>Previous artifacts</button><button class="btn" disabled={busy || !rows.next_cursor} onClick={() => setCursors(old => [...old, rows.next_cursor!])}>Next artifacts</button></div>
      <label class="field">Register a local file<input aria-label="Artifact ingest path" value={ingest} disabled={busy} maxLength={4096} onInput={e => setIngest(e.currentTarget.value)} /></label><p class="muted">Zeus hashes a regular file inside its managed download folders. Registration does not move the file or create a stored content copy.</p><button class="btn" disabled={busy || !ingest.trim()} onClick={() => run(async () => {
        const value = await api.ingest(ingest.trim(), key(`ingest:${ingest.trim()}`))
        if (alive.current) { retry.current = null; setSelected(value.artifact.id); setRefresh(n => n + 1); setNote(value.created ? 'Artifact registered.' : 'Existing artifact registration returned.') }
      })}>Register file</button>
    </section>
    {detailError && <Notice kind="error">{detailError}</Notice>}
    {record && <section class="runs-card" aria-label="Artifact details"><h2>{record.name || record.id}</h2><code>{record.id}</code><p>{fmtBytes(record.size_bytes)} · {record.state} · stored content {record.object.state} · retention {record.retention}</p><p>SHA-256: <code>{record.sha256}</code></p>{record.quarantined && <Notice>This artifact is quarantined. Lifecycle actions cannot release it.</Notice>}
      <div class="row-actions">{(['ancestors', 'descendants'] as const).map(direction => <button class="btn" disabled={busy} onClick={() => run(async () => { const value = await api.lineage(record.id, direction); if (alive.current) setLineage(value) })}>Show {direction}</button>)}</div>
      {lineage && <div aria-label="Artifact lineage"><p>{lineage.nodes.length} nodes{lineage.truncated ? ' · bounded or visibility-limited result' : ''}</p><div class="runs-scroll"><table><thead><tr><th>Depth</th><th>Record / content digest</th><th>Bytes</th><th>Links</th></tr></thead><tbody>{lineage.nodes.map((node, i) => <tr key={i}><td>{node.depth}</td><td><code>{node.artifact_id ?? node.blake3}</code></td><td>{fmtBytes(node.size_bytes)}</td><td>{node.links.map(link => `${link.role} → ${link.node}`).join(', ')}</td></tr>)}</tbody></table></div></div>}
      <details><summary>Full redacted record</summary><pre>{json(record)}</pre></details>
      <label class="field">Lifecycle action<select aria-label="Artifact lifecycle action" value={effect} disabled={busy || record.state !== 'active'} onChange={e => { changed(); setEffect(e.currentTarget.value) }}><option value="keep">Protect stored content</option><option value="evictable">Allow later content collection</option><option value="materialize">Copy verified stored content</option><option value="delete">Tombstone this record</option></select></label>
      {effect === 'materialize' ? <label class="field">New file path<input aria-label="Artifact copy destination" value={path} disabled={busy} maxLength={4096} onInput={e => { changed(); setPath(e.currentTarget.value) }} /></label> : effect !== 'delete' ? <label class="field">Protection deadline · optional ISO date and time<input aria-label="Artifact protection deadline" value={until} disabled={busy} maxLength={64} placeholder="2027-01-01T00:00:00Z" onInput={e => { changed(); setUntil(e.currentTarget.value) }} /></label> : <Notice>Tombstoning retains user files and shared content. It does not delete the downloaded file.</Notice>}
      <button class="btn" disabled={busy || record.state !== 'active'} onClick={inspect}>Preview artifact change</button>
      {preview && <div class="runs-card" aria-label="Artifact change preview"><pre>{json(preview.plan)}</pre><p>Review hash: <code>{preview.plan_hash}</code></p><button class="btn primary" disabled={busy || Date.parse(preview.plan.expires_at) <= Date.now()} onClick={apply}>{receipt?.approval_id ? 'Check approval and apply exact change' : preview.plan.requires_approval ? 'Request artifact approval' : 'Apply reviewed artifact change'}</button></div>}
      {receipt?.approval_id && <div><p>Waiting for human approval: <code>{receipt.approval_id}</code></p><button class="btn" disabled={busy} onClick={() => run(async () => { await api.review(receipt.approval_id!) })}>Review artifact approval</button></div>}
      {receipt?.path && <p>Verified copy: <code>{receipt.path}</code></p>}
    </section>}
    <section class="runs-card" aria-label="Deterministic organizer"><h2>Organize files</h2><p>Preview up to 16 ordered rules. The first matching rule chooses a file’s move, rename or trash operation. Applying or undoing a plan requires review in the human terminal.</p>
      <label class="field">Folder to organize<input aria-label="Organization folder" value={dir} disabled={busy} maxLength={4096} onInput={e => { ruleChanged(); setDir(e.currentTarget.value) }} /></label>
      <label class="field">Rules JSON<textarea class="runs-plan" aria-label="Organization rules JSON" value={rules} disabled={busy} maxLength={65536} spellcheck={false} onInput={e => { ruleChanged(); setRules(e.currentTarget.value) }} /></label>
      <p class="muted">Rules match extensions, bounded filename expressions, byte limits or age. Templates support {'{name}, {stem}, {ext}, {year}, {month}, {day}'}. Plain-language classification and conversion are not available here.</p>
      <div class="row-actions"><button class="btn" disabled={busy || !dir.trim()} onClick={organizePlan}>Preview organization</button><button class="btn primary" disabled={busy || !reviewedRules || !organization?.steps.length || !!organization.approval_id} onClick={organizeRequest}>Request organization approval</button></div>
      {operationError && <Notice kind="error">{operationError}</Notice>}{operationStatus && <p aria-label="Organization progress">{operationStatus.completed} completed · {operationStatus.skipped} skipped · {operationStatus.pending} pending{operationStatus.reason ? ` · ${operationStatus.reason}` : ''}{operationStatus.undone_by ? ` · inverse operation ${operationStatus.undone_by}` : ''}</p>}
      {organization && <div aria-label="Organization preview"><p>{organization.steps.length} steps · {operationStatus?.state ?? (organization.applied ? 'applied' : organization.approval_id ? 'awaiting human approval' : 'preview only')}</p><div class="runs-scroll"><table><thead><tr><th>Action</th><th>From</th><th>To</th></tr></thead><tbody>{organization.steps.map((step, i) => <tr key={i}><td>{step.op}</td><td><code>{step.from}</code></td><td><code>{step.to ?? 'application trash'}</code></td></tr>)}</tbody></table></div>{organization.operation_id && <p>Operation: <code>{organization.operation_id}</code></p>}{organization.approval_id && (!operationStatus || operationStatus.state === 'awaiting_approval') && <button class="btn" disabled={busy} onClick={() => run(async () => { await api.review(organization.approval_id!) })}>Review organization approval</button>}</div>}
      <label class="field">Completed organization operation to undo<input aria-label="Organization undo operation" value={undoId} disabled={busy} maxLength={128} onInput={e => setUndoId(e.currentTarget.value)} /></label><button class="btn" disabled={busy || !undoId.trim()} onClick={() => run(async () => { const value = await api.undo(undoId.trim(), key(`undo:${undoId.trim()}`)); if (alive.current) { setOrganization(value); setReviewedRules(null) } })}>Review undo plan</button>
    </section>
  </div>
}

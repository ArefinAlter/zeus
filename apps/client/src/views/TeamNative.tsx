import { useEffect, useRef, useState } from 'preact/hooks'
import { NATIVE } from '../backend/live'
import { teamApi as api, type TeamEnrollment, type TeamKind, type TeamRecord, type TeamStatus } from '../backend/team'
import { Notice, PageHead } from '../ui/components'
import '../styles/runs.css'

const message = (e: unknown) => e instanceof Error ? e.message : 'The team request failed.'
export function TeamNative() {
  const [status, setStatus] = useState<TeamStatus | null>(null), [error, setError] = useState(''), [note, setNote] = useState(''), [busy, setBusy] = useState(false)
  const [alias, setAlias] = useState('Zeus desktop'), [enrollment, setEnrollment] = useState<TeamEnrollment | null>(null)
  const [records, setRecords] = useState<TeamRecord[]>([]), [after, setAfter] = useState(0), [more, setMore] = useState(false)
  const [kind, setKind] = useState<TeamKind>('tags'), [id, setId] = useState(''), [revision, setRevision] = useState(0), [label, setLabel] = useState(''), [color, setColor] = useState('#6a72d6')
  const [parallel, setParallel] = useState(4), [speed, setSpeed] = useState(0), [start, setStart] = useState(0), [end, setEnd] = useState(1439), [tags, setTags] = useState('')
  const [digest, setDigest] = useState(''), [size, setSize] = useState(0), [source, setSource] = useState('http'), [assertion, setAssertion] = useState('')
  const alive = useRef(true), gate = useRef(false), retry = useRef<{ signature: string; key: string } | null>(null)
  useEffect(() => { alive.current = true; if (NATIVE) void api.status().then(v => { if (alive.current) setStatus(v) }).catch(e => { if (alive.current) setError(message(e)) }); return () => { alive.current = false } }, [])
  const run = (f: () => Promise<void>) => {
    if (gate.current) return
    gate.current = true; setBusy(true); setError(''); setNote('')
    void f().catch(e => { if (alive.current) setError(message(e)) }).finally(() => { gate.current = false; if (alive.current) { setBusy(false); void api.status().then(setStatus).catch(() => {}) } })
  }
  const key = (operation: string, value: unknown) => { const signature = operation + JSON.stringify(value); if (retry.current?.signature !== signature) retry.current = { signature, key: `desktop-${crypto.randomUUID()}` }; return retry.current.key }
  const pull = async (reset = false) => {
    const page = await api.changes(reset ? 0 : after)
    if (!alive.current) return
    setRecords(old => { const rows = new Map((reset ? [] : old).map(r => [`${r.kind}/${r.record_id}`, r])); for (const row of page.records) rows.set(`${row.kind}/${row.record_id}`, row); return [...rows.values()] })
    setAfter(page.records.at(-1)?.sequence ?? (reset ? 0 : after)); setMore(page.more)
  }
  const edit = (r: TeamRecord) => {
    setKind(r.kind); setId(r.record_id); setRevision(r.revision); setLabel(String(r.value?.label ?? ''))
    if (r.kind === 'tags') setColor(String(r.value?.color ?? '#6a72d6'))
    if (r.kind === 'workspace_labels') setTags(Array.isArray(r.value?.tags) ? r.value.tags.join(', ') : '')
    if (r.kind === 'queue_presets' && r.value) { setParallel(Number(r.value.parallel)); setSpeed(Number(r.value.bytes_per_second)); const s = r.value.schedule_utc as { start_minute: number; end_minute: number }; setStart(s.start_minute); setEnd(s.end_minute) }
    retry.current = null
  }
  const save = () => run(async () => {
    const value = kind === 'tags' ? { label, color } : kind === 'workspace_labels' ? { label, tags: tags.split(',').map(t => t.trim()).filter(Boolean) } : { label, parallel, bytes_per_second: speed, schedule_utc: { start_minute: start, end_minute: end } }
    await api.put(kind, id, revision, value, key('put', { kind, id, revision, value })); retry.current = null; setNote('Metadata shared. Pull and select the record again before another edit.'); await pull(true)
  })
  if (!NATIVE) return <div class="runs-page"><PageHead title="Team" sub="Optional team metadata and signed restrictions." /><Notice>Open the Windows desktop app to connect an administrator-provisioned team.</Notice></div>
  return <div class="runs-page">
    <PageHead title="Team" sub="Share selected labels and queue presets. Your files and transfer approvals stay local." />
    {error && <Notice kind="error">{error}</Notice>}{note && <Notice kind="ok">{note}</Notice>}
    {!status ? <p role="status">Loading team connection…</p> : !status.configured ? <Notice>No team is configured. Your administrator can provision <code>team/pin.json</code> under this Zeus installation’s daemon state folder, then restart Zeus. Ask for the verified service origin, tenant and public signing keys.</Notice> : <>
      <section class="runs-card"><h2>Connection</h2><p>{status.origin ?? 'Invalid trust configuration'} · {status.tenant_id ?? 'No trusted tenant'}</p>
        <p>Signed in: {status.connected ? 'Yes' : 'No'}. A new sign-in is required after restarting the desktop; the account token stays in native memory.</p>
        {status.policy_held && <Notice kind="warn">New team-covered operations are held until a trusted policy is refreshed. Pause, cancel and reading existing work remain available.</Notice>}
        {status.policy && <><p>Policy {status.policy.sequence} · expires {status.policy.expires_at}</p><p>{Object.entries(status.policy.features).map(([k, v]) => `${k.replaceAll('_', ' ')}: ${v ? 'allowed with local approval rules' : 'restricted'}`).join(' · ')}</p></>}
        <label class="field">Desktop label<input value={alias} maxLength={128} disabled={busy || !!enrollment} onInput={e => setAlias(e.currentTarget.value)} /></label>
        <div class="row-actions"><button class="btn primary" disabled={busy || !status.trusted || !!enrollment} onClick={() => run(async () => setEnrollment(await api.connect(alias)))}>Sign in with your team</button><button class="btn" disabled={busy || !status.connected} onClick={() => run(async () => { await api.refresh(); setNote('Signed policy refreshed.') })}>Refresh policy</button><button class="btn" disabled={busy || !status.connected} onClick={() => run(async () => { await api.rotate(); setNote('Session renewed within your identity provider’s login lifetime.') })}>Renew session</button><button class="btn" disabled={busy || !status.connected} onClick={() => run(async () => { await api.disconnect(); setRecords([]); setAfter(0); setAssertion('') })}>Sign out</button></div>
        {enrollment && <Notice><p>Compare code <strong>{enrollment.user_code}</strong> in your browser before approving this desktop. Choose tenant {status.tenant_id}.</p><p><code>{enrollment.verification_uri}</code></p><button class="btn" disabled={busy} onClick={() => run(async () => { await api.open() })}>Open team sign-in in browser</button><div class="row-actions"><button class="btn primary" disabled={busy} onClick={() => run(async () => { const result = await api.poll(); if (result.state === 'connected') { setEnrollment(null); setNote('Team connected.'); } else setNote('Waiting for your browser approval. Check again after completing sign-in.') })}>Check sign-in</button><button class="btn" disabled={busy} onClick={() => run(async () => { try { await api.cancel() } finally { setEnrollment(null) } })}>Cancel sign-in</button></div></Notice>}
      </section>
      <section class="runs-card"><h2>Shared metadata</h2><p>Only tags, workspace labels and queue presets are shared on request. Pulling a preset does not start a queue or change local settings. Concurrent edits require the latest revision.</p>
        <div class="row-actions"><button class="btn" disabled={busy || !status.connected} onClick={() => run(() => pull(true))}>Pull shared metadata</button><button class="btn" disabled={busy || !status.connected || !more} onClick={() => run(() => pull())}>Load next changes</button></div>
        <div class="runs-scroll"><table><thead><tr><th>Kind / ID</th><th>Value</th><th>Revision</th><th>Actions</th></tr></thead><tbody>{records.map(r => <tr key={`${r.kind}/${r.record_id}`}><td>{r.kind}<br /><code>{r.record_id}</code></td><td>{r.tombstone ? 'Deleted' : JSON.stringify(r.value)}</td><td>{r.revision}</td><td><button class="btn" disabled={busy || r.tombstone} onClick={() => edit(r)}>Edit</button><button class="btn" disabled={busy || r.tombstone || !status.connected} onClick={() => run(async () => { await api.delete(r, key('delete', r)); retry.current = null; await pull(true) })}>Delete shared metadata</button></td></tr>)}</tbody></table></div>
        <div class="runs-grid"><label class="field">Kind<select value={kind} disabled={busy} onChange={e => { setKind(e.currentTarget.value as TeamKind); setRevision(0) }}><option value="tags">Tag</option><option value="queue_presets">Queue preset</option><option value="workspace_labels">Workspace label</option></select></label><label class="field">Record ID<input value={id} maxLength={64} disabled={busy} onInput={e => { setId(e.currentTarget.value); setRevision(0) }} /></label><label class="field">Label<input value={label} maxLength={128} disabled={busy} onInput={e => setLabel(e.currentTarget.value)} /></label><label class="field">Expected revision<input type="number" min="0" step="1" value={revision} disabled={busy} onInput={e => setRevision(Number(e.currentTarget.value))} /></label>
          {kind === 'tags' && <label class="field">Color<input type="color" value={color} disabled={busy} onInput={e => setColor(e.currentTarget.value)} /></label>}
          {kind === 'workspace_labels' && <label class="field">Tag IDs, separated by commas<input value={tags} disabled={busy} onInput={e => setTags(e.currentTarget.value)} /></label>}
          {kind === 'queue_presets' && <><label class="field">Parallel downloads<input type="number" min="1" max="64" value={parallel} disabled={busy} onInput={e => setParallel(Number(e.currentTarget.value))} /></label><label class="field">Bytes per second · 0 means no limit<input type="number" min="0" value={speed} disabled={busy} onInput={e => setSpeed(Number(e.currentTarget.value))} /></label><label class="field">Start minute in UTC · 0–1439<input type="number" min="0" max="1439" value={start} disabled={busy} onInput={e => setStart(Number(e.currentTarget.value))} /></label><label class="field">End minute in UTC · 0–1439<input type="number" min="0" max="1439" value={end} disabled={busy} onInput={e => setEnd(Number(e.currentTarget.value))} /></label></>}
        </div><button class="btn primary" disabled={busy || !status.connected || !id || !label} onClick={save}>Share selected metadata</button>
      </section>
      <section class="runs-card"><h2>Signed artifact report</h2><p>The team signs your reported digest and size. This verifies the report’s signer; it does not prove that the server inspected the file or that the file is safe.</p><div class="runs-grid"><label class="field">SHA-256<input value={digest} maxLength={64} disabled={busy} onInput={e => setDigest(e.currentTarget.value.trim().toLowerCase())} /></label><label class="field">File size in bytes<input type="number" min="0" step="1" value={size} disabled={busy} onInput={e => setSize(Number(e.currentTarget.value))} /></label><label class="field">Source kind<select value={source} disabled={busy} onChange={e => setSource(e.currentTarget.value)}>{['http', 'cloud', 'torrent', 'artifact', 'unknown'].map(s => <option value={s}>{s}</option>)}</select></label></div><button class="btn" disabled={busy || !status.connected || digest.length !== 64} onClick={() => run(async () => { const result = await api.provenance(digest, size, source, key('provenance', { digest, size, source })); retry.current = null; setAssertion(JSON.stringify(result, null, 2)) })}>Sign and verify report</button>{assertion && <><pre>{assertion}</pre><button class="btn" onClick={() => { const url = URL.createObjectURL(new Blob([assertion], { type: 'application/json' })); const a = document.createElement('a'); a.href = url; a.download = 'zeus-team-provenance.json'; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000) }}>Save signed report</button></>}</section>
    </>}
  </div>
}

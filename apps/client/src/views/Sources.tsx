import { useEffect, useRef, useState } from 'preact/hooks'
import { NATIVE } from '../backend/live'
import { sourcesApi as api, type SourceAccount, type SourceOperation, type SourcePlan } from '../backend/sources'
import { fmtBytes } from '../model/units'
import { Notice, PageHead, StateLabel } from '../ui/components'
import '../styles/runs.css'

const errorText = (e: unknown) => e instanceof Error ? e.message : 'The source request failed.'
export function Sources() {
  const [accounts, setAccounts] = useState<SourceAccount[]>([])
  const [operations, setOperations] = useState<SourceOperation[]>([])
  const [accountId, setAccountId] = useState('')
  const account = accounts.find(a => a.account_id === accountId && !a.revoked)
  const [object, setObject] = useState('')
  const [revision, setRevision] = useState('')
  const [dest, setDest] = useState('')
  const [name, setName] = useState('')
  const [bytes, setBytes] = useState(1073741824)
  const [seconds, setSeconds] = useState(1800)
  const [connections, setConnections] = useState(8)
  const [torrent, setTorrent] = useState<{ base64: string; sha256: string; name: string } | null>(null)
  const [indices, setIndices] = useState('0')
  const [peers, setPeers] = useState('')
  const [names, setNames] = useState<Record<string, string>>({})
  const [plan, setPlan] = useState<SourcePlan | null>(null)
  const [files, setFiles] = useState<NonNullable<SourcePlan['torrent_files']>>([])
  const [error, setError] = useState('')
  const [readError, setReadError] = useState('')
  const [note, setNote] = useState('')
  const [busy, setBusy] = useState(false)
  const gate = useRef(false), alive = useRef(true)
  const pending = useRef<{ signature: string; key: string } | null>(null)
  const [refresh, setRefresh] = useState(0)
  useEffect(() => { alive.current = true; return () => { alive.current = false } }, [])
  useEffect(() => {
    if (!NATIVE) return
    let live = true, timer: ReturnType<typeof setTimeout> | undefined
    const tick = async () => {
      try {
        const [a, o] = await Promise.all([api.accounts(), api.operations()])
        if (live) { setAccounts(a.accounts); setOperations(o.operations); setReadError('') }
      } catch (e) { if (live) setReadError(errorText(e)) }
      finally { if (live) timer = setTimeout(() => void tick(), 4000) }
    }
    void tick()
    return () => { live = false; clearTimeout(timer) }
  }, [refresh])
  const changed = () => { setPlan(null); setNote(''); pending.current = null }
  const run = (f: () => Promise<void>) => {
    if (gate.current) return
    gate.current = true; setBusy(true); setError(''); setNote('')
    void f().catch(e => { if (alive.current) setError(errorText(e)) }).finally(() => { gate.current = false; if (alive.current) setBusy(false) })
  }
  const mutation = (operation: string, params: unknown) => {
    const signature = `${operation}:${JSON.stringify(params)}`
    if (pending.current?.signature !== signature) pending.current = { signature, key: `desktop-${crypto.randomUUID()}` }
    return pending.current.key
  }
  const chooseTorrent = (file: File | undefined) => run(async () => {
    changed(); setTorrent(null); setFiles([])
    if (!file) return
    if (file.size === 0 || file.size > 700 * 1024) throw new Error('Choose a .torrent metainfo file of at most 700 KiB.')
    const data = new Uint8Array(await file.arrayBuffer())
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', data))
    let binary = ''
    for (let offset = 0; offset < data.length; offset += 8192) binary += String.fromCharCode(...data.subarray(offset, offset + 8192))
    if (alive.current) { setTorrent({ name: file.name, sha256: [...digest].map(n => n.toString(16).padStart(2, '0')).join(''), base64: btoa(binary) }); setIndices('0'); setNames({}) }
  })
  const selectedIndices = () => {
    const result = indices.split(',').map(s => s.trim()).filter(Boolean).map(Number)
    if (!result.length || result.length > 4096 || result.some(n => !Number.isSafeInteger(n) || n < 0) || new Set(result).size !== result.length) throw new Error('Choose unique file indices, starting from zero.')
    return result
  }
  const inspect = () => run(async () => {
    setPlan(null)
    if (!account) throw new Error('Choose an available source account.')
    if (![bytes, seconds, connections].every(n => Number.isSafeInteger(n) && n > 0) || bytes > account.budget_ceiling.max_received_bytes || seconds * 1000 > account.budget_ceiling.timeout_ms || connections > 32) throw new Error('Use positive limits within this source’s configured byte/time ceiling and at most 32 connections.')
    const scope = account.config
    let target: unknown
    if (scope.provider === 's3') target = { provider: 's3', bucket: scope.bucket, key: object.trim(), version_id: revision.trim() || null }
    else if (scope.provider === 'hugging_face') target = { provider: 'hugging_face', repository_type: scope.repository_type, repository: scope.repository, commit: revision.trim(), path: object.trim() }
    else {
      if (!torrent) throw new Error('Choose the .torrent file to inspect.')
      target = { provider: 'torrent', metainfo_sha256: torrent.sha256, file_indices: selectedIndices(), initial_peers: peers.split(/\r?\n/).map(s => s.trim()).filter(Boolean) }
    }
    const params = { account_id: account.account_id, account_revision: account.revision, target, budget: { max_received_bytes: bytes, timeout_ms: seconds * 1000 }, metainfo_base64: scope.provider === 'torrent' ? torrent!.base64 : null }
    const result = await api.inspect({ ...params, idempotency_key: mutation('inspect', params) })
    if (alive.current) { setPlan(result); setFiles(result.torrent_files ?? []); pending.current = null }
  })
  const submit = () => run(async () => {
    if (!plan || !account || plan.account_revision !== account.revision) throw new Error('Review the source again after its configuration changed.')
    const selection = account.config.provider === 'torrent' ? selectedIndices() : []
    const mapping: Record<string, string> = {}
    if (selection.length > 1) for (const index of selection) {
      if (!names[index]?.trim()) throw new Error('Give each selected torrent file a destination name.')
      mapping[index] = names[index]!.trim()
    }
    const filename = name.trim() || (selection.length > 1 ? mapping[selection[0]!] : '')
    if (!filename) throw new Error('Enter a destination file name.')
    const params = { plan_id: plan.plan_id, dest_dir: dest.trim() || null, filename, max_connections: connections, torrent_filenames: mapping }
    const operation = await api.submit({ ...params, idempotency_key: mutation('submit', params) })
    if (alive.current) { setPlan(null); pending.current = null; setNote(`Transfer submitted: ${operation.operation_id}`); setRefresh(n => n + 1) }
  })
  const setup = (action: 'configure' | 'revoke', id = '') => run(async () => {
    await api.setup(action, id)
    if (alive.current) setNote('Zeus opened source setup in a separate terminal. Review the scope and choose which Zeus clients may use it, then confirm there.')
  })
  const control = (operation_id: string, action: 'pause' | 'resume' | 'cancel') => run(async () => {
    const params = { operation_id, action }
    await api.control({ ...params, idempotency_key: mutation('control', params) })
    if (alive.current) { pending.current = null; setRefresh(n => n + 1) }
  })
  if (!NATIVE) return <div class="page-body"><PageHead title="Connected sources" sub="Download from an S3 bucket, a pinned Hugging Face revision or selected torrent peers." /><Notice>Open Zeus desktop to connect sources on this computer.</Notice></div>
  return <div class="page-body runs-page">
    <PageHead title="Connected sources" sub="Review an exact file and download it within the source’s configured limits." />
    {error && <Notice kind="error">{error}</Notice>}{readError && <Notice kind="error">{readError} The last loaded accounts and operations remain visible.</Notice>}{note && <Notice>{note}</Notice>}
    <section class="runs-card" aria-label="Source accounts"><h2>Source accounts</h2><p>Connect an S3 bucket, a Hugging Face repository or a bounded set of torrent peers. Account setup reviews the scope, credentials and which Zeus clients can use it.</p><div class="row-actions"><button class="btn primary" disabled={busy} onClick={() => setup('configure')}>Connect a source</button><button class="btn" disabled={busy} onClick={() => setRefresh(n => n + 1)}>Refresh sources</button></div>
      {!accounts.length && !readError && <p>No source is available to this desktop yet. During setup, explicitly allow this installation’s app client.</p>}
    </section>
    <section class="runs-card" aria-label="Source download review"><h2>Download a source file</h2>
      <label class="field">Source account<select aria-label="Source account" disabled={busy} value={accountId} onChange={e => {
        changed(); setAccountId(e.currentTarget.value); setObject(''); setRevision(''); setFiles([]); setTorrent(null)
        const row = accounts.find(a => a.account_id === e.currentTarget.value)
        if (row) { setBytes(Math.min(1073741824, row.budget_ceiling.max_received_bytes)); setSeconds(Math.min(1800, Math.floor(row.budget_ceiling.timeout_ms / 1000))) }
      }}><option value="">Choose a connected source</option>{accounts.filter(a => !a.revoked).map(a => <option key={a.account_id} value={a.account_id}>{a.untrusted_label} · {a.config.provider}</option>)}</select></label>
      {account && <><p>Scope: {account.config.provider === 's3' ? `${account.config.bucket}/${account.config.key_prefix}` : account.config.provider === 'hugging_face' ? `${account.config.repository_type}: ${account.config.repository}` : `${account.config.max_peers} peers, ${fmtBytes(account.config.max_download_bytes_per_second)}/s maximum`}. Account revision {account.revision}.</p><button class="btn" disabled={busy} onClick={() => setup('revoke', account.account_id)}>Revoke source access…</button></>}
      {account?.config.provider === 'torrent' ? <>
        <Notice>Torrent acquisition uses explicit allowed peer IP addresses. Magnet links, trackers and DHT discovery are not supported; Zeus does not seed completed files.</Notice>
        <label class="field">Torrent metainfo<input aria-label="Torrent metainfo" type="file" accept=".torrent,application/x-bittorrent" disabled={busy} onChange={e => chooseTorrent(e.currentTarget.files?.[0])} /></label>
        {torrent && <p>{torrent.name} · SHA-256 <code>{torrent.sha256}</code></p>}
        <label class="field">Initial peers · one IP:port per line<textarea aria-label="Torrent peers" value={peers} disabled={busy} maxLength={16384} onInput={e => { changed(); setPeers(e.currentTarget.value) }} /></label>
        <label class="field">Selected file indices · comma-separated<input aria-label="Torrent file indices" value={indices} disabled={busy} maxLength={24000} onInput={e => { changed(); setIndices(e.currentTarget.value) }} /><small class="muted">The first review uses index 0. Use the returned file table to select other indices, then review again.</small></label>
        {!!files.length && <div class="runs-scroll"><table><thead><tr><th>Index</th><th>File in torrent</th><th>Bytes</th><th>Destination name for multiple files</th></tr></thead><tbody>{files.map(file => <tr key={file.index}><td>{file.index}</td><td>{file.untrusted_path.join('/')}</td><td>{fmtBytes(file.bytes)}</td><td><input aria-label={`Destination name for torrent file ${file.index}`} value={names[file.index] ?? ''} disabled={busy} maxLength={240} onInput={e => { setNames(old => ({ ...old, [file.index]: e.currentTarget.value })); pending.current = null }} /></td></tr>)}</tbody></table></div>}
      </> : <div class="runs-grid"><label class="field">{account?.config.provider === 's3' ? 'Object key' : 'Repository file path'}<input aria-label="Source object path" value={object} disabled={busy} maxLength={4096} onInput={e => { changed(); setObject(e.currentTarget.value) }} /></label><label class="field">{account?.config.provider === 's3' ? 'Version ID · optional' : 'Full 40-character commit'}<input aria-label="Source immutable revision" value={revision} disabled={busy} maxLength={512} onInput={e => { changed(); setRevision(e.currentTarget.value) }} /></label></div>}
      <div class="runs-grid"><label class="field">Maximum bytes received, including retries<input type="number" min="1" max={account?.budget_ceiling.max_received_bytes ?? 1099511627776} step="1" value={bytes} disabled={busy} onInput={e => { changed(); setBytes(Number(e.currentTarget.value)) }} /></label><label class="field">Maximum duration in seconds<input type="number" min="1" max="86400" step="1" value={seconds} disabled={busy} onInput={e => { changed(); setSeconds(Number(e.currentTarget.value)) }} /></label></div>
      <div class="row-actions"><button class="btn primary" disabled={busy || !account} onClick={inspect}>Review source</button>{busy && <span role="status">Waiting for the engine…</span>}</div>
      {plan && <div class="runs-card" aria-label="Reviewed source"><h3>Reviewed source</h3><p>{plan.untrusted_label} · {fmtBytes(plan.metadata.bytes)}</p><p>Revision: <code>{plan.metadata.immutable_revision}</code></p><p>Integrity: {plan.metadata.integrity_kind}{plan.metadata.sha256 ? ` · SHA-256 ${plan.metadata.sha256}` : ''}</p><p>Review expires {plan.expires_at}.</p>{plan.notices.map((notice, i) => <p key={i}>{notice}</p>)}
        <div class="runs-grid"><label class="field">Destination folder · blank uses the default<input aria-label="Source destination folder" value={dest} disabled={busy} onInput={e => { setDest(e.currentTarget.value); pending.current = null }} /></label><label class="field">Destination file name<input aria-label="Source destination file name" value={name} disabled={busy} maxLength={240} onInput={e => { setName(e.currentTarget.value); pending.current = null }} /></label><label class="field">Maximum HTTP connections<input aria-label="Source connections" type="number" min="1" max="32" value={connections} disabled={busy} onInput={e => { setConnections(Number(e.currentTarget.value)); pending.current = null }} /></label></div>
        <button class="btn primary" disabled={busy || !account || account.revision !== plan.account_revision || Date.parse(plan.expires_at) <= Date.now()} onClick={submit}>Download reviewed source</button>
      </div>}
    </section>
    <section class="runs-card" aria-label="Source operations"><h2>Source transfers</h2><p class="muted">The latest 100 operations owned by this desktop. HTTP sources also appear in Downloads. Received bytes include retries and can exceed final file size.</p>
      {!operations.length && !readError && <p>No source transfers yet.</p>}
      {operations.map(op => <article class="runs-card" key={op.operation_id}><h3>{op.untrusted_label}</h3><p><StateLabel state={op.state} /> · received {fmtBytes(op.received_bytes)} of {fmtBytes(op.budget.max_received_bytes)} permitted · uploaded payload {fmtBytes(op.payload_uploaded_bytes)}</p><code>{op.operation_id}</code>{op.error_code && <Notice kind="error">{op.error_code}</Notice>}{op.output_sha256 && <p>Verified output SHA-256: <code>{op.output_sha256}</code></p>}{op.files.map(file => <p key={file.index}>{file.filename} · {fmtBytes(file.bytes)} · SHA-256 <code>{file.sha256}</code></p>)}<div class="row-actions"><button class="btn" disabled={busy || !['queued', 'running'].includes(op.state)} onClick={() => control(op.operation_id, 'pause')}>Pause source</button><button class="btn" disabled={busy || op.state !== 'paused'} onClick={() => control(op.operation_id, 'resume')}>Resume source</button><button class="btn" disabled={busy || !['queued', 'running', 'paused'].includes(op.state)} onClick={() => control(op.operation_id, 'cancel')}>Cancel source</button></div></article>)}
    </section>
  </div>
}

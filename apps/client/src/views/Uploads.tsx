import { useEffect, useState } from 'preact/hooks'
import { NATIVE } from '../backend/live'
import { call } from '../backend/remote'
import { fmtBytes } from '../model/units'
import { PageHead, Notice } from '../ui/components'
import '../styles/uploads.css'

type Account = {
  account_id: string
  label: string
  provider: string
  destination: string
  revoked: boolean
  capabilities: { staged_upload: boolean; stream_upload: boolean; remote_import: boolean }
}
type Plan = {
  plan_id: string
  plan_hash: string
  account_id: string
  source_redacted: string
  destination: string
  remote_name: string
  size_bytes: number
  expected_sha256?: string
  supported: boolean
  unsupported_reason?: string
}
type Delivery = { approval_id?: string; committed_bytes: number; delivery_id: string; state: string; reason?: string; plan: Plan }
const id = () => `desktop-${crypto.randomUUID()}`

export function Uploads({ initialPath = '', initialName = '' }: { initialPath?: string; initialName?: string }) {
  const [accounts, setAccounts] = useState<Account[]>([])
  const [deliveries, setDeliveries] = useState<Delivery[]>([])
  const [account, setAccount] = useState('')
  const [kind, setKind] = useState<'local_file' | 'completed_job' | 'public_url'>('local_file')
  const [source, setSource] = useState(initialPath)
  const [name, setName] = useState(initialName)
  const [bytes, setBytes] = useState('1073741824')
  const [plan, setPlan] = useState<Plan | null>(null)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const refresh = async () => {
    const [a, d] = await Promise.all([
      call<{ accounts: Account[] }>('GET', '/native/v1/cloud/accounts'),
      call<{ deliveries: Delivery[] }>('GET', '/native/v1/cloud/deliveries'),
    ])
    setAccounts(a.accounts ?? [])
    setDeliveries(d.deliveries ?? [])
  }
  useEffect(() => {
    if (!NATIVE) return
    let alive = true
    const tick = () => {
      if (alive) void refresh().catch((e) => setError(e instanceof Error ? e.message : 'Transfers could not be read.'))
    }
    tick()
    const timer = setInterval(tick, 3000)
    return () => {
      alive = false
      clearInterval(timer)
    }
  }, [])
  const run = async (f: () => Promise<void>) => {
    setBusy(true)
    setError('')
    setMessage('')
    try {
      await f()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'The transfer request failed.')
    } finally {
      setBusy(false)
    }
  }
  const review = () =>
    void run(async () => {
      const amount = Number(bytes)
      if (!account || !source.trim() || !name.trim() || !Number.isSafeInteger(amount) || amount <= 0)
        throw new Error('Choose an account, source, file name and a positive byte ceiling.')
      const input =
        kind === 'local_file'
          ? { kind, path: source.trim() }
          : kind === 'completed_job'
            ? { kind, job_id: source.trim() }
            : { kind, url: source.trim(), size_bytes: amount }
      const result = await call<{ plan: Plan }>('POST', '/native/v1/cloud/plan', {
        account_id: account,
        source: input,
        mode: 'staged_upload',
        remote_name: name.trim(),
        max_bytes: amount,
        max_seconds: 1800,
        allow_unbounded_remote: false,
        idempotency_key: id(),
      })
      setPlan(result.plan)
    })
  const openReview = (action: 'configure' | 'revoke' | 'review' | 'approve', target = '') => void run(async () => {
    await call('POST', '/native/v1/cloud/review-terminal', { action, id: target })
    setMessage('Zeus opened a review terminal for this installation. Read the scope and confirm there. This window never sends a decision.')
  })
  const change = () => {
    setPlan(null)
    setMessage('')
  }
  if (!NATIVE)
    return (
      <>
        <PageHead title="Uploads" sub="Send files to connected drives and file hosts." />
        <Notice kind="info">Open Zeus desktop to manage your files and upload from this computer.</Notice>
      </>
    )
  return (
    <div class="page-body uploads-page">
      <PageHead
        title="Uploads"
        sub="Review a destination and exact source before requesting approval. Transfer controls and progress stay in the engine."
      />
      <p class="muted">
        Connect Google Drive, Yandex Disk, Baidu Netdisk or a configured rclone remote with account setup below. A
        destination must support uploading; support varies by connector. Account credentials stay in the engine.
      </p>
      <div class="row-actions"><button class="btn" disabled={busy} onClick={()=>openReview('configure')}>Connect destination</button><button class="btn" disabled={busy} onClick={()=>openReview('review')}>Review pending approvals</button></div>
      {error ? <Notice kind="error">{error}</Notice> : null}
      {message ? <Notice kind="info">{message}</Notice> : null}
      {!accounts.some((a) => !a.revoked) ? (
        <Notice kind="info">
          No destination is connected yet. Choose Connect destination below; Zeus opens account setup for this installation. Local
          files must be in a folder included under Settings, Search and index.
        </Notice>
      ) : null}
      <form
        class="upload-form"
        onSubmit={(e) => {
          e.preventDefault()
          review()
        }}
      >
        <label class="field">
          Destination
          <select
            aria-label="Destination"
            value={account}
            onChange={(e) => {
              setAccount(e.currentTarget.value)
              change()
            }}
          >
            <option value="">Choose a connected account</option>
            {accounts
              .filter((a) => !a.revoked)
              .map((a) => (
                <option value={a.account_id}>
                  {a.label} · {a.provider} · {a.destination}
                </option>
              ))}
          </select>
        </label>
        <label class="field">
          Source type
          <select
            aria-label="Source type"
            value={kind}
            onChange={(e) => {
              setKind(e.currentTarget.value as typeof kind)
              setSource('')
              change()
            }}
          >
            <option value="local_file">File on this computer</option>
            <option value="completed_job">Completed download</option>
            <option value="public_url">Public URL</option>
          </select>
        </label>
        <label class="field">
          {kind === 'local_file' ? 'Full file path' : kind === 'completed_job' ? 'Completed job ID' : 'Public URL'}
          <input
            value={source}
            onInput={(e) => {
              setSource(e.currentTarget.value)
              change()
            }}
            required
            spellcheck={false}
          />
        </label>
        <label class="field">
          Destination file name
          <input
            value={name}
            onInput={(e) => {
              setName(e.currentTarget.value)
              change()
            }}
            required
          />
        </label>
        <label class="field">
          {kind === 'public_url' ? 'Exact source size in bytes' : 'Maximum bytes to upload'}
          <input
            type="number"
            min="1"
            step="1"
            value={bytes}
            onInput={(e) => {
              setBytes(e.currentTarget.value)
              change()
            }}
            required
          />
        </label>
        <button class="btn primary" disabled={busy || !account} type="submit">
          Review upload
        </button>
      </form>
      {plan ? (
        <section class="upload-card" aria-label="Upload review">
          <h2>Review upload</h2>
          <p>
            {plan.source_redacted} → {plan.destination} / {plan.remote_name}
          </p>
          <p>
            {fmtBytes(plan.size_bytes)} · source SHA-256: <code>{plan.expected_sha256 ?? 'not reported'}</code>
          </p>
          {!plan.supported ? (
            <Notice kind="error">
              This destination cannot perform this upload: {plan.unsupported_reason ?? 'unsupported capability'}.
            </Notice>
          ) : (
            <button
              class="btn primary"
              disabled={busy}
              onClick={() =>
                void run(async () => {
                  await call('POST', '/native/v1/cloud/submit', { plan_id: plan.plan_id, plan_hash: plan.plan_hash, idempotency_key: id() })
                  setPlan(null)
                  setMessage('Approval requested. Choose Review pending approvals to open the review terminal. Uploading starts only after your decision.')
                  await refresh()
                })
              }
            >
              Request human approval
            </button>
          )}
        </section>
      ) : null}
      <section class="upload-section">
        <h2>Transfers</h2>
        {deliveries.length === 0 ? (
          <p class="muted">No uploads yet.</p>
        ) : (
          deliveries.map((d) => (
            <article class="upload-card">
              <strong>{d.plan.remote_name}</strong>
              <span>
                {d.state} · {fmtBytes(d.committed_bytes)} / {fmtBytes(d.plan.size_bytes)} → {d.plan.destination}
              </span>
              {d.reason ? <span class="muted">{d.reason}</span> : null}
              <div class="row-actions">
                {d.approval_id && d.state === 'awaiting_approval' ? <button class="btn small" disabled={busy} onClick={()=>openReview('approve',d.approval_id)}>Review approval</button> : null}
                {(['pause', 'resume', 'cancel'] as const).map((action) => (
                  <button
                    class="btn small"
                    disabled={
                      busy ||
                      (action === 'pause'
                        ? !['running', 'queued', 'reconciling'].includes(d.state)
                        : action === 'resume'
                          ? !['paused', 'failed'].includes(d.state)
                          : ['completed', 'canceled'].includes(d.state))
                    }
                    onClick={() =>
                      void run(async () => {
                        await call('POST', '/native/v1/cloud/control', { delivery_id: d.delivery_id, action, idempotency_key: id() })
                        await refresh()
                      })
                    }
                  >
                    {action.charAt(0).toUpperCase() + action.slice(1)}
                  </button>
                ))}
              </div>
            </article>
          ))
        )}
      </section>
      <section class="upload-section">
        <h2>Connected destinations</h2>
        {accounts.map((a) => (
          <article class="upload-card">
            <strong>{a.label}</strong>
            <p>
              {a.provider} · {a.destination}
              {a.revoked ? ' · revoked' : ''}
            </p>
            {!a.revoked ? <button class="btn small" disabled={busy} onClick={()=>openReview('revoke',a.account_id)}>Review disconnect</button> : null}
            <p class="muted">
              Upload: {a.capabilities.staged_upload ? 'supported' : 'unavailable'} · Streaming:{' '}
              {a.capabilities.stream_upload ? 'supported' : 'unavailable'} · Provider fetch:{' '}
              {a.capabilities.remote_import ? 'supported' : 'unavailable'}
            </p>
          </article>
        ))}
      </section>
    </div>
  )
}

// Which backend this client talks to, and the HTTP calls it makes.
//
// * scripted: the in-page sample backend (store.ts); no network at all. Offline builds and tests.
// * hosted: the Zeus pre-alpha on the web. `/api/v1` is zeus-web-bridge in front of a real zeusd; `/svc/v1` is the
//   pre-alpha service for what zeusd does not implement yet. Same origin, session cookie.
// * local: your own zeusd through zeus-web-bridge on 127.0.0.1, with the token from its bridge.token file. The
//   pre-alpha service does not exist locally, so client-only settings stay on this device.
//
// The website build is always `hosted`. The desktop build starts on `hosted` at the pre-alpha's address and can be
// switched in Settings, Backend. Nothing here ever sends a provider key: those stay in memory.

export type BackendKind = 'scripted' | 'hosted' | 'local'

export const HOSTED_URL = 'https://zeus.103.191.50.32.sslip.io'
export const LOCAL_URL = 'http://127.0.0.1:18545'

const BUILD = (import.meta.env.VITE_ZEUS_BACKEND as string | undefined) ?? 'scripted'
const CHOICE_KEY = 'zeus.backend.v1'
const SESSION_KEY = 'zeus.hosted.session.v1'

export interface BackendChoice {
  kind: BackendKind
  /** Base URL without a trailing slash; '' for the page's own origin. */
  base: string
}

function storage(): Storage | null {
  try {
    return window.localStorage
  } catch {
    return null
  }
}

/** The desktop app may switch backends; the website copy may not. */
export const SWITCHABLE = BUILD === 'desktop'

function resolve(): BackendChoice {
  if (BUILD === 'hosted') return { kind: 'hosted', base: '' }
  if (BUILD !== 'desktop') return { kind: 'scripted', base: '' }
  try {
    const raw = storage()?.getItem(CHOICE_KEY)
    if (raw) {
      const c = JSON.parse(raw) as Partial<BackendChoice>
      if (c.kind === 'scripted') return { kind: 'scripted', base: '' }
      if (c.kind === 'local' && typeof c.base === 'string' && /^http:\/\/(127\.0\.0\.1|localhost|\[::1\]):\d{2,5}$/.test(c.base)) return { kind: 'local', base: c.base }
      if (c.kind === 'hosted') return { kind: 'hosted', base: HOSTED_URL }
    }
  } catch {
    // Fall back to the default.
  }
  return { kind: 'hosted', base: HOSTED_URL }
}

const CURRENT = resolve()

export function backend(): BackendChoice {
  return CURRENT
}

/** Desktop only: remember a new backend and reload the window. */
export function switchBackend(next: BackendChoice): void {
  try {
    storage()?.setItem(CHOICE_KEY, JSON.stringify(next))
  } catch {
    // Without storage the choice holds until the window closes.
  }
  window.location.reload()
}

// ---------------------------------------------------------------- credentials

/** The local bridge token: typed in Settings, held in memory only. */
let localToken = ''
/** The desktop app's hosted sandbox id (a bearer, since the webview's origin differs from the server's). */
let hostedToken = (() => {
  if (BUILD !== 'desktop') return ''
  try {
    return storage()?.getItem(SESSION_KEY) ?? ''
  } catch {
    return ''
  }
})()

export function setLocalToken(t: string): void {
  localToken = t.trim()
}

export function hasLocalToken(): boolean {
  return localToken.length > 0
}

export class RemoteError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status: number,
  ) {
    super(message)
  }
}

function headers(json: boolean): Record<string, string> {
  const h: Record<string, string> = {}
  if (json) h['content-type'] = 'application/json'
  if (CURRENT.kind === 'local' && localToken) h.authorization = `Bearer ${localToken}`
  if (CURRENT.kind === 'hosted' && BUILD === 'desktop') {
    h['x-zeus-client'] = 'desktop'
    if (hostedToken) h.authorization = `Bearer ${hostedToken}`
  }
  return h
}

async function request<T>(path: string, method = 'GET', body?: unknown, retried = false): Promise<T> {
  let r: Response
  try {
    r = await fetch(`${CURRENT.base}${path}`, {
      method,
      headers: headers(body !== undefined),
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
      credentials: CURRENT.base ? 'omit' : 'same-origin',
      cache: 'no-store',
    })
  } catch {
    throw new RemoteError('client.offline', CURRENT.kind === 'local' ? 'The local bridge does not answer. Start zeusd and zeus-web-bridge, then reconnect.' : 'The Zeus pre-alpha server does not answer. Check your connection and reload.', 0)
  }
  let data: unknown = null
  try {
    data = await r.json()
  } catch {
    data = null
  }
  if (!r.ok) {
    const e = (data as { error?: { code?: string; message?: string } } | null)?.error
    const code = e?.code ?? `http.${r.status}`
    if (r.status === 401 && !retried && CURRENT.kind === 'hosted' && (code === 'bridge.session_required' || code === 'svc.session_expired' || path.startsWith('/svc/'))) {
      await session(true)
      return request<T>(path, method, body, true)
    }
    throw new RemoteError(code, e?.message ?? `The server answered ${r.status}.`, r.status)
  }
  return data as T
}

// ---------------------------------------------------------------- the bridge (zeusd)

export interface SessionInfo {
  mode: 'hosted' | 'local'
  bridge_version: string
  created_ms: number
  expires_ms: number | null
  quota: { files: number; bytes: number } | null
  used: { files: number; bytes: number; jobs: number }
  fixture_hosts: string[]
  root_label: string
  token?: string
}

let sessionPromise: Promise<SessionInfo> | null = null

export function session(force = false): Promise<SessionInfo> {
  if (!sessionPromise || force) {
    if (force && BUILD === 'desktop') hostedToken = ''
    sessionPromise = request<SessionInfo>('/api/v1/session', 'POST', {}, true).then((s) => {
      if (s.token && BUILD === 'desktop') {
        hostedToken = s.token
        try {
          storage()?.setItem(SESSION_KEY, s.token)
        } catch {
          // The sandbox lasts until the window closes.
        }
      }
      return s
    })
    sessionPromise.catch(() => {
      sessionPromise = null
    })
  }
  return sessionPromise
}

export interface DaemonStatus {
  daemon_version: string
  capabilities: string[]
  permissions: string[]
  head_seq: number
  pending_approvals: number
  kill_switch_engaged: boolean
  journal_ok: boolean
  uptime_ms: number
}

export interface RemoteJob {
  id: string
  state: string
  filename?: string | null
  url_redacted: string
  bytes_done: number
  bytes_total?: number | null
  rate_ewma?: number | null
  eta_s?: number | null
  quarantined: boolean
  source_kind: string
  sha256?: string | null
  created_at: string
  updated_at: string
  path?: string
  error_code?: string
}

export interface RemoteFile {
  path: string
  size_bytes: number
  modified_at: string
  sha256?: string
  quarantined?: boolean
  partial?: boolean
  job_id?: string
}

export interface RemoteArtifact {
  id: string
  state: string
  name?: string
  size_bytes?: number
  sha256?: string
  blake3?: string
  media_type?: string | null
  quarantined?: boolean
  created_at?: string
  job_id?: string | null
  run_id?: string | null
}

export interface RemoteApproval {
  id: string
  kind: string
  state: string
  created_at: string
  expires_at: string
  decided_at?: string
  request_hash?: string
  allowed_scopes?: string[]
  reason_codes?: string[]
  summary?: Record<string, unknown>
  job_id?: string
  run_id?: string
  origin?: string
  revision?: number
}

export interface RemoteRun {
  run_id: string
  status: string
  plan_hash?: string
  pages_fetched?: number
  files_enqueued?: number
  files_completed?: number
  files_failed?: number
  bytes_done?: number
  started_at?: string
  finished_at?: string
  created_at?: string
  updated_at?: string
  elapsed_ms?: number
  possibly_incomplete?: boolean
}

export interface PreviewResult {
  review_id: string | null
  ready: boolean
  items: { index: number; url_redacted?: string; filename?: string | null; destination?: string | null; approval_needed?: boolean; warnings?: string[] | null; error_code?: string | null }[]
  refused: { index: number; reason: string }[]
  bytes?: number
}

export interface EnqueueResult {
  replayed: boolean
  jobs: { job_id: string | null; state: string | null; approval_id: string | null; error_code: string | null }[]
}

export const api = {
  status: () => request<{ daemon: DaemonStatus | null; failure: { code: string; message: string } | null; updated_ms: number }>('/api/v1/status'),
  jobs: () => request<{ items: RemoteJob[]; updated_ms: number; failure: { code: string; message: string } | null }>('/api/v1/jobs'),
  preview: (urls: string[], subdir: string) => request<PreviewResult>('/api/v1/links/preview', 'POST', { urls, subdir }),
  enqueue: (review_id: string, idempotency_key: string) => request<EnqueueResult>('/api/v1/links/enqueue', 'POST', { review_id, idempotency_key }),
  control: (id: string, action: 'pause' | 'resume' | 'cancel' | 'retry', idempotency_key: string) => request<{ replayed: boolean; job_id: string; state: string }>(`/api/v1/jobs/${encodeURIComponent(id)}/control`, 'POST', { action, idempotency_key }),
  files: () => request<{ items: RemoteFile[] }>('/api/v1/files'),
  artifacts: () => request<{ items: RemoteArtifact[] }>('/api/v1/artifacts'),
  approvals: () => request<{ items: RemoteApproval[]; decide: string }>('/api/v1/approvals'),
  runs: () => request<{ items: RemoteRun[] }>('/api/v1/runs'),
  fixtures: () => request<{ hosts: string[]; samples: { url: string; note: string }[] }>('/api/v1/fixtures'),
}

// ---------------------------------------------------------------- the pre-alpha service (hosted only)

export interface IndexView {
  generation: number
  last_scan: number | null
  files: { path: string; size: number; mtime: number; is_dir: boolean }[]
}

export const svc = {
  available: () => CURRENT.kind === 'hosted',
  state: () => request<{ slices: Record<string, { value: unknown; updated: number }>; first_seen: number | null; ttl_ms: number }>('/svc/v1/state'),
  put: (slice: string, value: unknown) => request<{ ok: boolean }>(`/svc/v1/state/${slice}`, 'PUT', value),
  clear: () => request<{ ok: boolean }>('/svc/v1/state', 'DELETE'),
  index: () => request<IndexView>('/svc/v1/index'),
  rescan: () => request<IndexView>('/svc/v1/index/rescan', 'POST', {}),
  check: (urls: string[]) => request<{ items: { url: string; check: 'online' | 'offline' | 'needs_login'; size: number | null; status: number }[] }>('/svc/v1/links/check', 'POST', { urls }),
  sitePreview: (req: unknown) => request<unknown>('/svc/v1/sitegrab/preview', 'POST', req),
  approvals: () => request<{ items: unknown[] }>('/svc/v1/approvals'),
  createApproval: (a: unknown) => request<unknown>('/svc/v1/approvals', 'POST', a),
  decide: (id: string, d: { decision: 'approve' | 'deny'; scope?: string; seen_hash: string; armed_ms: number }) => request<unknown>(`/svc/v1/approvals/${encodeURIComponent(id)}/decide`, 'POST', d),
}

/** A fresh retry key for one mutation. */
export function retryKey(): string {
  return `k${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`
}

// Which backend this client talks to, and the HTTP calls it makes.
//
// * scripted: the in-page sample backend (store.ts); no network at all. Offline builds and tests.
// * hosted: the Zeus pre-alpha on the web. `/api/v1` is zeus-web-bridge in front of a real zeusd; `/svc/v1` is the
//   pre-alpha service for what zeusd does not implement yet. Same origin, session cookie.
// * local: your own zeusd through zeus-web-bridge on 127.0.0.1, with the token from its bridge.token file. The
//   pre-alpha service does not exist locally, so client-only settings stay on this device.
// * native: the desktop app's own engine. The app (Rust) provisions its own Zeus installation, runs the bundled zeusd,
//   and answers the same routes through one closed Tauri command; the credential never reaches this page. In a plain
//   browser the same routes are reachable through a loopback harness (test builds only).
//
// The installed desktop starts on `native`; a desktop page opened outside Tauri uses the local bridge.
// The retired public-demo choice is migrated to that default. Hosted builds remain for isolated development.
// Nothing here ever sends a provider key.

import { invoke } from '@tauri-apps/api/core'

export type BackendKind = 'scripted' | 'hosted' | 'local' | 'native'

export const LOCAL_URL = 'http://127.0.0.1:18545'

const BUILD = (import.meta.env.VITE_ZEUS_BACKEND as string | undefined) ?? 'scripted'
/** Inside the desktop app's window (Tauri injects its internals before the page runs). */
export const IN_APP = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
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

/** Test builds only: the engine's loopback harness, named in the page address (`?harness=URL&token=TOKEN`). */
const HARNESS = (() => {
  if (BUILD !== 'harness') return null
  try {
    const q = new URLSearchParams(window.location.search)
    const base = q.get('harness') ?? ''
    const token = q.get('token') ?? ''
    if (/^http:\/\/(127\.0\.0\.1|localhost):\d{2,5}$/.test(base) && /^[0-9a-f]{32}$/.test(token)) return { base, token }
  } catch {
    // No harness named.
  }
  return null
})()

function resolve(): BackendChoice {
  if (BUILD === 'hosted') return { kind: 'hosted', base: '' }
  if (BUILD === 'harness') return HARNESS ? { kind: 'native', base: HARNESS.base } : { kind: 'scripted', base: '' }
  if (BUILD !== 'desktop') return { kind: 'scripted', base: '' }
  try {
    const raw = storage()?.getItem(CHOICE_KEY)
    if (raw) {
      const c = JSON.parse(raw) as Partial<BackendChoice>
      if (c.kind === 'scripted') return { kind: 'scripted', base: '' }
      if (c.kind === 'native' && IN_APP) return { kind: 'native', base: '' }
      if (c.kind === 'local' && typeof c.base === 'string' && /^http:\/\/(127\.0\.0\.1|localhost|\[::1\]):\d{2,5}$/.test(c.base)) return { kind: 'local', base: c.base }
      // Migrate the retired public-demo choice back to the installed engine.
      if (c.kind === 'hosted') return IN_APP ? { kind: 'native', base: '' } : { kind: 'local', base: LOCAL_URL }
    }
  } catch {
    // Fall back to the default.
  }
  return IN_APP ? { kind: 'native', base: '' } : { kind: 'local', base: LOCAL_URL }
}

const CURRENT = resolve()
/** The native routes travel over Tauri's IPC inside the app and over the loopback harness in a test browser. */
const OVER_IPC = CURRENT.kind === 'native' && IN_APP

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
  if (CURRENT.kind === 'native' && HARNESS) h.authorization = `Bearer ${HARNESS.token}`
  if (CURRENT.kind === 'hosted' && BUILD === 'desktop') {
    h['x-zeus-client'] = 'desktop'
    if (hostedToken) h.authorization = `Bearer ${hostedToken}`
  }
  return h
}

async function viaIpc<T>(path: string, method: string, body?: unknown): Promise<T> {
  try {
    return (await invoke('backend_call', { route: `${method} ${path}`, body: body ?? null })) as T
  } catch (e) {
    const f = e as { code?: string; message?: string } | string
    const code = typeof f === 'object' && f?.code ? f.code : 'client.error'
    const message = typeof f === 'string' ? f : (f?.message ?? 'The Zeus engine did not answer.')
    throw new RemoteError(code, message, 0)
  }
}

async function request<T>(path: string, method = 'GET', body?: unknown, retried = false): Promise<T> {
  if (OVER_IPC) return viaIpc<T>(path, method, body)
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
    throw new RemoteError('client.offline', CURRENT.kind === 'native' ? 'The Zeus engine does not answer.' : CURRENT.kind === 'local' ? 'The local bridge does not answer. Start zeusd and zeus-web-bridge, then reconnect.' : 'The Zeus pre-alpha server does not answer. Check your connection and reload.', 0)
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

/** One call of the closed route table (native app: Tauri IPC; test builds: the loopback harness). For feature modules. */
export const call = <T>(method: string, path: string, body?: unknown): Promise<T> => request<T>(path, method, body)

// ---------------------------------------------------------------- the bridge (zeusd)

export interface SessionInfo {
  mode: 'hosted' | 'local' | 'native'
  bridge_version: string
  created_ms: number
  expires_ms: number | null
  quota: { files: number; bytes: number } | null
  used: { files: number; bytes: number; jobs: number }
  fixture_hosts: string[]
  root_label: string
  token?: string
  /** Native only: every download folder, the path separator, the platform and the app's own folders. */
  roots?: string[]
  sep?: string
  platform?: string
  home?: string
  logs?: string
  contained?: boolean
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

export interface RemoteHistory {
  entries: { seq: number; ts: string; job_id: string; kind: string; state: string | null }[]
  replaying: boolean
  history_lost: boolean
  limited: boolean
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
  last_error?: { code: string } | null
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
  items: { index: number; url_redacted?: string; filename?: string | null; destination?: string | null; approval_needed?: boolean; warnings?: string[] | null; error_code?: string | null; error_message?: string | null }[]
  refused: { index: number; reason: string }[]
  bytes?: number
}

export interface EnqueueResult {
  replayed: boolean
  jobs: { job_id: string | null; state: string | null; approval_id: string | null; error_code: string | null; error_message?: string | null }[]
}

export const api = {
  history: () => request<RemoteHistory>('/native/v1/events'),
  status: () => request<{ daemon: DaemonStatus | null; failure: { code: string; message: string } | null; updated_ms: number }>('/api/v1/status'),
  jobs: () => request<{ items: RemoteJob[]; updated_ms: number; failure: { code: string; message: string } | null }>('/api/v1/jobs'),
  preview: (urls: string[], subdir: string) => request<PreviewResult>('/api/v1/links/preview', 'POST', { urls, subdir }),
  /** Native: a batch with per-item folder, name, expected hash, queue and priority. */
  previewItems: (items: NativeItem[]) => request<PreviewResult>('/api/v1/links/preview', 'POST', { items }),
  job: (id: string) => request<{ job: JobDetail }>(`/api/v1/jobs/${encodeURIComponent(id)}`),
  enqueue: (review_id: string, idempotency_key: string, queue?: string | null) => request<EnqueueResult>('/api/v1/links/enqueue', 'POST', { review_id, idempotency_key, ...(queue ? { queue } : {}) }),
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

export interface LinkCheck {
  url: string
  check: 'online' | 'offline' | 'needs_login'
  size: number | null
  status: number
  /** Native: what the probe learned. */
  name?: string | null
  name_source?: string | null
  resumable?: 'verified' | 'unsupported' | 'unknown' | null
  content_type?: string | null
  redirects?: number | null
  final_url?: string | null
  interstitial?: boolean | null
  error_code?: string | null
  error_message?: string | null
}

export const svc = {
  available: () => CURRENT.kind === 'hosted' || CURRENT.kind === 'native',
  state: () => request<{ slices: Record<string, { value: unknown; updated: number }>; first_seen: number | null; ttl_ms: number }>('/svc/v1/state'),
  put: (slice: string, value: unknown) => request<{ ok: boolean }>(`/svc/v1/state/${slice}`, 'PUT', value),
  clear: () => request<{ ok: boolean }>('/svc/v1/state', 'DELETE'),
  index: () => request<IndexView>('/svc/v1/index'),
  rescan: () => request<IndexView>('/svc/v1/index/rescan', 'POST', {}),
  check: (urls: string[]) => request<{ items: LinkCheck[] }>('/svc/v1/links/check', 'POST', { urls }),
  sitePreview: (req: unknown) => request<unknown>('/svc/v1/sitegrab/preview', 'POST', req),
  approvals: () => request<{ items: unknown[] }>('/svc/v1/approvals'),
  createApproval: (a: unknown) => request<unknown>('/svc/v1/approvals', 'POST', a),
  decide: (id: string, d: { decision: 'approve' | 'deny'; scope?: string; seen_hash: string; armed_ms: number }) => request<unknown>(`/svc/v1/approvals/${encodeURIComponent(id)}/decide`, 'POST', d),
}

/** One download of a native batch. */
export interface NativeItem {
  url: string
  dir?: string
  filename?: string
  expected_sha256?: string
  queue?: string | null
  priority?: 'low' | 'normal' | 'high'
  overwrite?: 'rename' | 'fail' | 'replace'
  max_connections?: number
}

/** zeusd's full view of one job (redacted): the probe, retry counters, the result and quarantine reasons. */
export interface JobDetail {
  probe?: { status: number; range_support: string; content_type?: string | null; total_bytes?: number | null; redirects?: number; final_url_redacted?: string | null; filename?: { value: string; source: string }; validators?: { etag?: string | null; last_modified?: string | null }; requires_session?: boolean; interstitial_suspected?: boolean }
  retry?: { attempt: number; consecutive_failures: number; next_attempt_at?: string | null }
  result?: { path: string; size_bytes: number; sha256: string; blake3: string; detected_mime?: string | null; completed_at: string }
  quarantine_reasons?: string[]
  quarantined?: boolean
  spec?: { overwrite?: string; max_connections?: number; expected_sha256?: string | null; labels?: Record<string, string> }
  error?: { code: string; message: string } | null
  progress?: { segments_active?: number; rate_ewma?: number }
}

/** The engine's own routes (the desktop app only). */
export interface EngineInfo {
  status: { phase: 'starting' | 'ready' | 'restarting' | 'failed' | 'stopped'; message: string; restarts: number; version: string | null; pid: number | null; adopted: boolean }
  home: string
  logs: string
  state: string
  config: string
  bin: string
  env: Record<string, string>
  settings: EngineSettings
  app_version: string
}

export interface EngineSettings {
  write_roots: string[]
  concurrent_jobs: number
  connections_per_job: number
  connections_per_host: number
  connect_s: number
  read_s: number
  probe_s: number
  tombstone_days: number
  proxy: 'system' | 'http'
  proxy_host: string
  proxy_port: number
  proxy_bypass: string
}

/** zeusd's queue API, as `queues.list` returns it. */
export interface QueueView {
  id: string
  name: string
  kind: 'download' | 'sync'
  sync_every_hours?: number
  sync_files?: { job_id: string; pending_job_id: string | null; last_checked_ms: number | null; next_check_ms: number; status: string }[]
  max_concurrent: number
  retries: number
  retry_delay_s: number
  schedule: { start_enabled: boolean; start_time: string; repeat: 'once' | 'days'; start_date: string; days: number[]; stop_enabled: boolean; stop_time: string }
  on_finish: 'nothing' | 'notify' | 'stop_limiter' | 'close_app' | 'sleep'
  limit_kib_s: number | null
  builtin: boolean
  running: boolean
  last_started_ms: number | null
  next_start_ms: number | null
  next_stop_ms?: number | null
  waiting: number
  active: number
  order: string[]
  held_job_ids: string[]
  finished_at_ms: number | null
  finish_action_pending: boolean
}

export interface QueuesList {
  queues: QueueView[]
  limiter: { enabled: boolean; limit_kib_s: number; schedule_enabled: boolean; from: string; to: string; days: number[]; active_now?: boolean }
  defaults: { timezone: 'local' | 'utc'; missed_window: 'skip' | 'run_once'; stop_behaviour: 'pause' | 'finish_current'; default_queue: string; network_mode?: 'auto' | 'unmetered' | 'metered'; pause_on_metered?: boolean }
  network?: { cost: 'unknown' | 'unmetered' | 'metered'; paused: boolean; held_jobs: number }
  now_ms: number
  utc_offset_min?: number
  kill_switch_engaged: boolean
}

export const native = {
  engine: () => request<EngineInfo>('/native/v1/engine'),
  restart: () => request<{ ok: boolean }>('/native/v1/engine/restart', 'POST', {}),
  engineSettings: () => request<EngineSettings>('/native/v1/engine/settings'),
  putEngineSettings: (s: EngineSettings) => request<{ ok: boolean; restart_needed: boolean }>('/native/v1/engine/settings', 'PUT', s),
  queues: () => request<QueuesList>('/native/v1/queues'),
  queueOp: (op: 'put' | 'delete' | 'control' | 'configure', body: unknown) => request<Record<string, unknown>>(`/native/v1/queues/${op}`, 'POST', body),
  open: (path: string, action: 'open' | 'reveal') => request<{ ok: boolean }>('/native/v1/open', 'POST', { path, action }),
  finishAction: (action: 'close_app' | 'sleep') => request<{ ok: boolean }>('/native/v1/finish-action', 'POST', { action }),
}

/** A fresh retry key for one mutation. */
export function retryKey(): string {
  return `k${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`
}

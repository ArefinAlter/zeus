// The live backend: the same store, fed by a real zeusd through zeus-web-bridge, with everything zeusd does not
// implement yet kept by the pre-alpha service (hosted) or on this device (local).
//
// From zeusd (through the bridge, with the restricted gui credential): job pages and their states, link preview and
// enqueue of a reviewed batch, pause, resume, retry and cancel, files under the download folder, artifact integrity
// facts (SHA-256, BLAKE3), the approvals this identity may see (listing only) and workflow reports.
//
// From the pre-alpha service, per visitor sandbox: queues and their schedules, the speed limiter's settings, every
// Settings section, categories and file types, link-grabber lists and online checks, site-grabber walks over the
// sample sites and their plans, agent threads, skills, connector choices, bookmarks and search history, the search
// index over the sandbox's real files, and the approvals for plans and file moves that the service itself holds.
//
// Queued downloads that have not started are held here (and stored with the queue) until their queue starts; zeusd
// starts a download as soon as it is enqueued. Provider keys are never stored or sent: they are removed from Settings
// before anything is saved.

import type { Approval, FileEntry, Job, JobState, Scope } from '../model/types.ts'
import { categoryOf } from '../model/categories.ts'
import { bindCode } from '../model/approvals.ts'
import { getPrefs, hydratePrefs, subscribePrefs } from '../lib/prefs.ts'
import type { RemoteApproval, RemoteArtifact, RemoteFile, RemoteJob, RemoteRun, SessionInfo } from './remote.ts'
import { RemoteError, api, backend, retryKey, session, svc } from './remote.ts'
import type { Settings } from './settings.ts'
import { ulid } from './rand.ts'

export const LIVE = backend().kind !== 'scripted'
export const HOSTED = backend().kind === 'hosted'

/** Display-only facts the client keeps about a zeusd job. */
export interface JobMeta {
  queue?: string | null
  priority?: Job['priority']
  source?: Job['source']
  project?: string
  description?: string
  hidden?: boolean
}

/** A download waiting in a queue that has not started; zeusd has not seen it yet. */
export interface PendingItem {
  id: string
  url: string
  display: string
  host: string
  queryWithheld: boolean
  name: string
  dir: string
  queue: string | null
  added: number
  source: Job['source']
  size: number | null
  project?: string
  description?: string
  /** Set when sending it failed; the row shows as failed with this reason. */
  failed?: { code: string; message: string }
  sending?: boolean
}

export interface LiveInfo {
  kind: 'hosted' | 'local'
  connected: boolean
  loaded: boolean
  failure: { code: string; message: string } | null
  session: SessionInfo | null
  daemon: {
    daemon_version: string
    capabilities: string[]
    permissions: string[]
    pending_approvals: number
    kill_switch_engaged: boolean
    journal_ok: boolean
    uptime_ms: number
  } | null
  runs: RemoteRun[]
  artifacts: RemoteArtifact[]
  lastJobs: number
}

// The store is imported lazily through these hooks to avoid an import cycle at module evaluation.
interface StoreHooks {
  get: () => import('./store.ts').State
  set: (patch: Partial<import('./store.ts').State>) => void
  toast: typeof import('./store.ts').toast
  subscribe: (f: () => void) => () => void
  defaultDir: (c: Job['category']) => string
  queueName: (id: string | null) => string
}

let S: StoreHooks

const ERRORS: Record<string, [string, string]> = {
  'http.forbidden': ['The server refused the file (403). A signed link that expired looks like this.', 'Get a fresh link from the site and add it again.'],
  'http.url_expired': ['The link expired.', 'Get a fresh link from the site and add it again.'],
  'http.not_found': ['The server has no such file (404).', 'Check the address.'],
  'http.gone': ['The file was removed from the server (410).', 'Nothing to retry; find another source.'],
  'http.server_error': ['The server failed (5xx).', 'Retry later.'],
  'http.unavailable': ['The server is busy (503) and asked Zeus to wait.', 'zeusd retries on its own.'],
}

const STATUS: Record<string, string> = { '403': 'http.forbidden', '404': 'http.not_found', '410': 'http.gone', '500': 'http.server_error', '502': 'http.server_error', '503': 'http.unavailable' }

function errorOf(code: string | undefined): Job['error'] | undefined {
  if (!code) return undefined
  const status = /^http\.status_(\d{3})$/.exec(code)?.[1]
  const known = ERRORS[status ? (STATUS[status] ?? '') : code] ?? (code.startsWith('http.') ? ['The server answered with an error.', 'Retry, or check the address.'] : ['zeusd reported a problem with this download.', 'Retry, or remove it from the list.'])
  return { code, message: known[0], next: known[1] }
}

function hostOf(url: string): string {
  try {
    return new URL(url.replace('?…', '')).hostname
  } catch {
    return ''
  }
}

function baseName(url: string): string {
  try {
    const u = new URL(url.replace('?…', ''))
    return decodeURIComponent(u.pathname.split('/').filter(Boolean).pop() ?? u.hostname)
  } catch {
    return url
  }
}

function rootLabel(): string {
  return S.get().live?.session?.root_label ?? 'Downloads'
}

/** `Documents/x.pdf` -> dir `Downloads\Documents`, name `x.pdf`. */
function splitPath(p: string): { dir: string; name: string } {
  const parts = p.split('/').filter(Boolean)
  const name = parts.pop() ?? p
  return { dir: [rootLabel(), ...parts].join('\\'), name }
}

/** The folder inside the sandbox for a display dir such as `Downloads\Documents`. */
export function subdirOf(dir: string): string {
  const root = rootLabel()
  const rest = dir === root ? '' : dir.startsWith(root + '\\') ? dir.slice(root.length + 1) : ''
  return rest.split('\\').filter(Boolean).join('/')
}

// ---------------------------------------------------------------- mapping zeusd views to the client's model

const remoteJobs = new Map<string, RemoteJob>()
let remoteFiles: RemoteFile[] = []
let indexFiles: { path: string; size: number; mtime: number; is_dir: boolean }[] = []
const lastState = new Map<string, string>()

function toJob(r: RemoteJob, prev: Job | undefined): Job {
  const s = S.get()
  const meta = s.jobMeta[r.id] ?? {}
  const name = r.filename ?? baseName(r.url_redacted)
  const file = remoteFiles.find((f) => f.job_id === r.id && !f.partial) ?? remoteFiles.find((f) => f.job_id === r.id)
  const where = file ? splitPath(file.path) : null
  const size = r.bytes_total ?? null
  const state = r.state as JobState
  const art = s.live?.artifacts.find((a) => a.job_id === r.id)
  const active = ['probing', 'downloading', 'verifying', 'finalizing'].includes(state)
  const events = prev?.events ?? [{ t: Date.parse(r.created_at), text: 'Enqueued in zeusd' }]
  const job: Job = {
    id: r.id,
    name,
    url: r.url_redacted.replace('?…', ''),
    host: hostOf(r.url_redacted),
    queryWithheld: r.url_redacted.includes('?…'),
    size,
    done: r.bytes_done,
    rate: active ? (r.rate_ewma ?? 0) : 0,
    state,
    category: categoryOf(name, s.settings.categories),
    queue: meta.queue ?? null,
    added: Date.parse(r.created_at),
    ...(state === 'completed' ? { finished: Date.parse(r.updated_at) } : {}),
    dir: where?.dir ?? S.defaultDir(categoryOf(name, s.settings.categories)),
    source: meta.source ?? { kind: 'app', label: 'You, in Zeus' },
    segments: size ? [{ start: 0, end: size, done: Math.min(size, r.bytes_done), state: r.bytes_done >= size ? 'done' : active ? 'active' : 'pending' }] : [],
    connections: s.settings.downloads.connectionsPerJob,
    resumable: 'unknown',
    priority: meta.priority ?? 'normal',
    ...(r.sha256 ? { sha256: r.sha256 } : {}),
    ...(art?.blake3 ? { blake3: art.blake3 } : {}),
    ...(r.quarantined ? { quarantined: true, quarantineReasons: ['zeusd quarantine'] } : {}),
    ...(state === 'failed' || state === 'retry_scheduled' ? { error: errorOf(r.error_code) ?? errorOf('job.failed') } : {}),
    ...(meta.project ? { grabberProject: meta.project } : {}),
    ...(meta.description ? { description: meta.description } : {}),
    events,
  }
  if (state === 'verifying' && size) job.hashed = r.bytes_done
  return job
}

function pendingJob(p: PendingItem): Job {
  const s = S.get()
  const category = categoryOf(p.name, s.settings.categories)
  return {
    id: p.id,
    name: p.name,
    url: p.display,
    host: p.host,
    queryWithheld: p.queryWithheld,
    size: p.size,
    done: 0,
    rate: 0,
    state: p.failed ? 'failed' : p.sending ? 'probing' : 'queued',
    category,
    queue: p.queue,
    added: p.added,
    dir: p.dir,
    source: p.source,
    segments: [],
    connections: s.settings.downloads.connectionsPerJob,
    resumable: 'unknown',
    priority: 'normal',
    ...(p.failed ? { error: { code: p.failed.code, message: p.failed.message, next: 'Fix the address, or remove it from the list.' } } : {}),
    ...(p.project ? { grabberProject: p.project } : {}),
    ...(p.description ? { description: p.description } : {}),
    events: [{ t: p.added, text: p.sending ? 'Sending to zeusd' : p.queue ? `Waiting in ${S.queueName(p.queue)}; zeusd receives it when the queue starts` : 'Waiting to be sent to zeusd' }],
  }
}

/** Rebuild the job list from zeusd's last page and the pending items. */
export function rebuildJobs(): void {
  const s = S.get()
  const prev = new Map(s.jobs.map((j) => [j.id, j]))
  const real: Job[] = []
  const finished: Job[] = []
  for (const r of remoteJobs.values()) {
    if (s.jobMeta[r.id]?.hidden) continue
    let j = toJob(r, prev.get(r.id))
    const was = lastState.get(r.id)
    if (was !== undefined && was !== r.state) {
      j = { ...j, events: [...j.events.slice(-30), { t: Date.now(), text: `zeusd: ${r.state.replace('_', ' ')}` }] }
      if (r.state === 'completed' || r.state === 'failed') finished.push(j)
    }
    lastState.set(r.id, r.state)
    real.push(j)
  }
  real.sort((a, b) => b.added - a.added)
  const jobs = [...s.pending.map(pendingJob), ...real]
  S.set({ jobs })
  for (const f of finished) {
    if (f.state === 'completed') S.toast({ kind: f.quarantined ? 'warn' : 'ok', title: f.quarantined ? 'Download complete, quarantined' : 'Download complete', desc: `${f.name}`, important: false, topic: 'complete', to: `#/downloads?job=${f.id}`, sound: 'done' })
    else S.toast({ kind: 'bad', title: 'Download failed', desc: `${f.name}: ${f.error?.message ?? ''}`, important: true, topic: 'failure', to: `#/downloads?job=${f.id}`, sound: 'fail' })
  }
  if (finished.some((f) => f.state === 'completed')) void refreshFiles(true)
}

function rebuildFiles(): void {
  const byPath = new Map(remoteFiles.map((f) => [f.path, f]))
  const out: FileEntry[] = []
  let id = 1
  const seenDirs = new Set<string>()
  const addDir = (rel: string, mtime: number) => {
    if (!rel || seenDirs.has(rel)) return
    seenDirs.add(rel)
    const { dir, name } = splitPath(rel)
    out.push({ id: id++, name, dir, isFolder: true, size: 0, modified: mtime })
  }
  const source = indexFiles.length || HOSTED ? indexFiles : remoteFiles.map((f) => ({ path: f.path, size: f.size_bytes, mtime: Date.parse(f.modified_at), is_dir: false }))
  for (const f of source) {
    if (f.is_dir) {
      addDir(f.path, f.mtime)
      continue
    }
    if (f.path.endsWith('.zpart')) continue
    const parts = f.path.split('/')
    for (let i = 1; i < parts.length; i++) addDir(parts.slice(0, i).join('/'), f.mtime)
    const r = byPath.get(f.path)
    const { dir, name } = splitPath(f.path)
    const host = r?.job_id ? remoteJobs.get(r.job_id)?.url_redacted : undefined
    out.push({
      id: id++,
      name,
      dir,
      isFolder: false,
      size: f.size,
      modified: f.mtime,
      ...(r?.sha256 ? { sha256: r.sha256 } : {}),
      ...(r?.job_id ? { jobId: r.job_id } : {}),
      ...(r?.quarantined ? { quarantined: true } : {}),
      ...(host ? { host: hostOf(host) } : {}),
    })
  }
  S.set({ files: out })
}

export function approvalFromDaemon(a: RemoteApproval): Approval {
  const hash = a.request_hash ?? ''
  const summary = a.summary ?? {}
  const count = typeof summary.files === 'number' ? summary.files : typeof summary.count === 'number' ? summary.count : 1
  return {
    id: a.id,
    state: (['pending', 'approved', 'denied', 'expired', 'canceled'].includes(a.state) ? a.state : 'pending') as Approval['state'],
    facts: {
      t: 'zeus.approval.facts.v1',
      approval_id: a.id,
      revision: a.revision ?? 1,
      kind: a.kind as Approval['facts']['kind'],
      website: a.origin ?? 'zeusd',
      requester: { kind: 'agent', label: 'zeus-web-bridge (this sandbox)' },
      for: `${count} file${count === 1 ? '' : 's'}`,
      files: [],
      destination: rootLabel(),
      allowed_scopes: (a.allowed_scopes ?? []) as Scope[],
      confirmation: 'gesture',
      issued_at: a.created_at,
      expires_at: a.expires_at,
    },
    requestHash: hash,
    bindCode: hash ? bindCode(hash) : '',
    jobIds: a.job_id ? [a.job_id] : [],
    heldBy: 'zeusd',
  }
}

// ---------------------------------------------------------------- polling

let daemonApprovals: Approval[] = []
let serviceApprovals: Approval[] = []

function mergeApprovals(): void {
  const s = S.get()
  const local = s.approvals.filter((a) => a.heldBy !== 'zeusd' && !serviceApprovals.some((x) => x.id === a.id))
  S.set({ approvals: [...serviceApprovals, ...local, ...daemonApprovals] })
}

function connected(failure: LiveInfo['failure']): void {
  const s = S.get()
  if (!s.live) return
  if (s.live.connected === !failure && (s.live.failure?.code ?? null) === (failure?.code ?? null)) return
  S.set({ live: { ...s.live, connected: !failure, failure } })
}

async function pollJobs(): Promise<void> {
  try {
    const r = await api.jobs()
    remoteJobs.clear()
    for (const j of r.items) remoteJobs.set(j.id, j)
    connected(r.failure)
    rebuildJobs()
  } catch (e) {
    connected(e instanceof RemoteError ? { code: e.code, message: e.message } : { code: 'client.error', message: 'The backend could not be read.' })
  }
}

let filesBusy = false
export async function refreshFiles(withIndex = false): Promise<void> {
  if (filesBusy) return
  filesBusy = true
  try {
    const [f, ix] = await Promise.all([api.files(), withIndex && HOSTED ? svc.rescan() : Promise.resolve(null)])
    remoteFiles = f.items
    if (ix) {
      indexFiles = ix.files
      S.set({ index: { state: 'current', progress: 1, generation: ix.generation, lastScan: ix.last_scan ?? Date.now() } })
    }
    rebuildFiles()
    rebuildJobs()
  } catch {
    // The next refresh tries again.
  } finally {
    filesBusy = false
  }
}

async function pollSlow(): Promise<void> {
  try {
    const [st, arts, aps, runs] = await Promise.all([api.status(), api.artifacts(), api.approvals(), api.runs()])
    const s = S.get()
    if (s.live) S.set({ live: { ...s.live, daemon: st.daemon, artifacts: arts.items, runs: runs.items } })
    daemonApprovals = aps.items.map(approvalFromDaemon)
    if (HOSTED) {
      const held = await svc.approvals()
      serviceApprovals = (held.items as Approval[]).map((a) => ({ ...a, heldBy: 'service' as const }))
    }
    mergeApprovals()
  } catch {
    // Shown by the job poll's connection state.
  }
}

// ---------------------------------------------------------------- persistence of what zeusd does not hold

const SLICES = ['queues', 'limiter', 'settings', 'skills', 'threads', 'bookmarks', 'history', 'connectors', 'projects', 'grabber', 'jobmeta', 'pending', 'killswitch'] as const
type Slice = (typeof SLICES)[number]

function stripKeys(settings: Settings): Settings {
  return { ...settings, providers: settings.providers.map(({ key: _k, ...p }) => p) }
}

function sliceValue(name: Slice): unknown {
  const s = S.get()
  switch (name) {
    case 'queues':
      return s.queues
    case 'limiter':
      return s.limiter
    case 'settings':
      return stripKeys(s.settings)
    case 'skills':
      return s.skills
    case 'threads':
      return s.threads
    case 'bookmarks':
      return s.bookmarks
    case 'history':
      return s.history
    case 'connectors':
      return s.connectorsAdded
    case 'projects':
      return s.projects
    case 'grabber':
      return s.grabber.map(({ check, ...g }) => ({ ...g, check: check === 'checking' ? 'unchecked' : check })).filter((g) => !g.queryWithheld)
    case 'jobmeta':
      return s.jobMeta
    case 'pending':
      // An address with a query string is never stored on a server; it stays in this window only.
      return s.pending.filter((p) => !p.queryWithheld).map(({ sending: _s, ...p }) => p)
    case 'killswitch':
      return s.killSwitch
  }
}

function sliceRef(name: Slice): unknown {
  const s = S.get()
  const refs: Record<Slice, unknown> = {
    queues: s.queues,
    limiter: s.limiter,
    settings: s.settings,
    skills: s.skills,
    threads: s.threads,
    bookmarks: s.bookmarks,
    history: s.history,
    connectors: s.connectorsAdded,
    projects: s.projects,
    grabber: s.grabber,
    jobmeta: s.jobMeta,
    pending: s.pending,
    killswitch: s.killSwitch,
  }
  return refs[name]
}

const saved = new Map<Slice, unknown>()
const dirty = new Set<string>()
let saveTimer: ReturnType<typeof setTimeout> | undefined

function localKey(name: string): string {
  return `zeus.local.${name}.v1`
}

function flush(): void {
  saveTimer = undefined
  const names = [...dirty]
  dirty.clear()
  for (const name of names) {
    const value = name === 'prefs' ? getPrefs() : sliceValue(name as Slice)
    if (HOSTED) {
      svc.put(name, value).catch((e: unknown) => {
        if (e instanceof RemoteError && e.code === 'svc.quota') S.toast({ kind: 'warn', title: 'Not saved', desc: e.message, important: true, topic: 'system' })
      })
    } else {
      try {
        window.localStorage.setItem(localKey(name), JSON.stringify(value))
      } catch {
        // Kept for this window only.
      }
    }
  }
}

function markDirty(name: string): void {
  dirty.add(name)
  if (saveTimer === undefined) saveTimer = setTimeout(flush, 700)
}

function watchSlices(): void {
  for (const n of SLICES) saved.set(n, sliceRef(n))
  S.subscribe(() => {
    if (!S.get().live?.loaded) return
    for (const n of SLICES) {
      const ref = sliceRef(n)
      if (ref !== saved.get(n)) {
        saved.set(n, ref)
        markDirty(n)
      }
    }
  })
  if (HOSTED)
    subscribePrefs(() => {
      if (S.get().live?.loaded) markDirty('prefs')
    })
}

const isArr = Array.isArray
const isObj = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v)

function applySlices(slices: Record<string, unknown>): void {
  const s = S.get()
  const patch: Partial<import('./store.ts').State> = {}
  if (isArr(slices.queues) && slices.queues.length) patch.queues = slices.queues as typeof s.queues
  if (isObj(slices.limiter)) patch.limiter = { ...s.limiter, ...(slices.limiter as object) }
  if (isObj(slices.settings)) {
    const stored = slices.settings as Partial<Settings>
    const merged = { ...s.settings } as Record<string, unknown>
    for (const [k, v] of Object.entries(stored)) if (k in merged) merged[k] = isObj(v) && isObj(merged[k]) ? { ...(merged[k] as object), ...v } : v
    patch.settings = merged as unknown as Settings
  }
  if (isArr(slices.skills)) patch.skills = slices.skills as typeof s.skills
  if (isArr(slices.threads)) patch.threads = slices.threads as typeof s.threads
  if (isArr(slices.bookmarks)) patch.bookmarks = slices.bookmarks as typeof s.bookmarks
  if (isArr(slices.history)) patch.history = slices.history as string[]
  if (isArr(slices.connectors)) patch.connectorsAdded = slices.connectors as string[]
  if (isArr(slices.projects)) patch.projects = slices.projects as typeof s.projects
  if (isArr(slices.grabber)) patch.grabber = slices.grabber as typeof s.grabber
  if (isObj(slices.jobmeta)) patch.jobMeta = slices.jobmeta as Record<string, JobMeta>
  if (isArr(slices.pending)) patch.pending = slices.pending as PendingItem[]
  if (typeof slices.killswitch === 'boolean') patch.killSwitch = slices.killswitch
  S.set(patch)
  if (isObj(slices.prefs)) hydratePrefs(slices.prefs)
}

async function loadSlices(): Promise<void> {
  if (HOSTED) {
    const r = await svc.state()
    const values: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(r.slices)) values[k] = v.value
    applySlices(values)
    return
  }
  const values: Record<string, unknown> = {}
  for (const n of SLICES) {
    try {
      const raw = window.localStorage.getItem(localKey(n))
      if (raw) values[n] = JSON.parse(raw)
    } catch {
      // Nothing stored.
    }
  }
  applySlices(values)
}

// ---------------------------------------------------------------- sending downloads to zeusd

/** Hosted only: the hosts this sandbox may download from. */
export function hostRefusal(host: string): string | null {
  const hosts = S.get().live?.session?.fixture_hosts ?? []
  if (!HOSTED || !hosts.length) return null
  if (hosts.some((h) => h.toLowerCase() === host.toLowerCase())) return null
  return `The hosted pre-alpha downloads only from its sample file hosts (${hosts.join(', ')}), never from the internet. Install Zeus to download from any site.`
}

function patchPending(ids: string[], f: (p: PendingItem) => PendingItem | null): void {
  const s = S.get()
  S.set({ pending: s.pending.map((p) => (ids.includes(p.id) ? f(p) : p)).filter((p): p is PendingItem => p !== null) })
}

/** Preview, then enqueue, a group of pending items; each group shares one destination folder. */
export async function send(ids: string[]): Promise<void> {
  const s = S.get()
  const items = s.pending.filter((p) => ids.includes(p.id) && !p.sending)
  if (!items.length) return
  patchPending(items.map((p) => p.id), (p) => {
    const { failed: _f, ...rest } = p
    return { ...rest, sending: true }
  })
  rebuildJobs()
  const groups = new Map<string, PendingItem[]>()
  for (const p of items) groups.set(p.dir, [...(groups.get(p.dir) ?? []), p])
  for (const [dir, group] of groups) {
    const fail = (code: string, message: string, which: PendingItem[] = group) => {
      patchPending(which.map((p) => p.id), (p) => {
        const { sending: _s, ...rest } = p
        return { ...rest, failed: { code, message } }
      })
      S.toast({ kind: 'bad', title: 'Not started', desc: message, important: true, topic: 'failure' })
    }
    try {
      const pv = await api.preview(group.map((p) => p.url), subdirOf(dir))
      if (pv.refused.length) {
        for (const r of pv.refused) {
          const p = group[r.index]
          if (p) fail('policy.refused', r.reason, [p])
        }
        const ok = group.filter((_, i) => !pv.refused.some((r) => r.index === i))
        patchPending(ok.map((p) => p.id), (p) => {
          const { sending: _s, ...rest } = p
          return rest
        })
        if (ok.length) void send(ok.map((p) => p.id))
        continue
      }
      if (!pv.ready || !pv.review_id) {
        fail('job.preview_failed', 'zeusd could not plan these downloads. Check the addresses.')
        continue
      }
      const r = await api.enqueue(pv.review_id, retryKey())
      const meta = { ...S.get().jobMeta }
      r.jobs.forEach((j, i) => {
        const p = group[i]
        if (!j.job_id || !p) return
        meta[j.job_id] = { queue: p.queue, source: p.source, ...(p.project ? { project: p.project } : {}), ...(p.description ? { description: p.description } : {}) }
      })
      const started = new Set(group.filter((_, i) => r.jobs[i]?.job_id).map((p) => p.id))
      S.set({ jobMeta: meta, pending: S.get().pending.filter((p) => !started.has(p.id)) })
      const failedHere = group.filter((p) => !started.has(p.id))
      if (failedHere.length) fail('job.enqueue_failed', 'zeusd refused some of these downloads.', failedHere)
      await pollJobs()
    } catch (e) {
      fail(e instanceof RemoteError ? e.code : 'client.error', e instanceof RemoteError ? e.message : 'The download could not be sent to zeusd.')
    }
  }
}

/** One of the four controls, through the bridge. */
export async function control(ids: string[], action: 'pause' | 'resume' | 'cancel' | 'retry'): Promise<number> {
  let n = 0
  for (const id of ids) {
    try {
      await api.control(id, action, retryKey())
      n++
    } catch (e) {
      const msg = e instanceof RemoteError ? e.message : 'zeusd did not answer.'
      S.toast({ kind: 'warn', title: `Could not ${action}`, desc: msg, important: false, topic: 'system' })
    }
  }
  await pollJobs()
  return n
}

export function newPendingId(): string {
  return ulid('pend', Date.now())
}

// ---------------------------------------------------------------- start

let started = false

export function startLive(hooks: StoreHooks): void {
  S = hooks
  if (started) return
  started = true
  const kind = backend().kind === 'local' ? 'local' : 'hosted'
  S.set({ live: { kind, connected: false, loaded: false, failure: null, session: null, daemon: null, runs: [], artifacts: [], lastJobs: 0 } })
  watchSlices()
  const boot = async () => {
    try {
      const sess = await session()
      const s = S.get()
      const root = sess.root_label
      S.set({
        live: { ...(s.live as LiveInfo), session: sess, connected: true, failure: null },
        settings: {
          ...s.settings,
          saveTo: { ...s.settings.saveTo, root, tempDir: `${root}\\.zeus-partial`, writeRoots: [root] },
          index: { ...s.settings.index, roots: [{ path: root, kind: 'folder', include: true, monitorChanges: false, rescanEveryHours: null }] },
        },
      })
      try {
        await loadSlices()
      } catch {
        S.toast({ kind: 'warn', title: 'Settings not loaded', desc: 'The pre-alpha service did not answer; changes are kept for this window.', important: false, topic: 'system' })
      }
      // The sandbox's root is the server's, whatever was stored.
      const cur = S.get()
      S.set({
        settings: { ...cur.settings, saveTo: { ...cur.settings.saveTo, root, writeRoots: [root] }, index: { ...cur.settings.index, roots: [{ path: root, kind: 'folder', include: true, monitorChanges: false, rescanEveryHours: null }] } },
        live: { ...(cur.live as LiveInfo), loaded: true },
      })
      for (const n of SLICES) saved.set(n, sliceRef(n))
      if (HOSTED) {
        try {
          const ix = await svc.index()
          indexFiles = ix.files
          S.set({ index: { state: 'current', progress: 1, generation: ix.generation, lastScan: ix.last_scan ?? Date.now() } })
        } catch {
          // Built from zeusd's file list instead.
        }
      }
      await Promise.all([pollJobs(), refreshFiles(false), pollSlow()])
    } catch (e) {
      connected(e instanceof RemoteError ? { code: e.code, message: e.message } : { code: 'client.error', message: 'The backend could not be reached.' })
      setTimeout(() => void boot(), 5000)
      return
    }
    let n = 0
    setInterval(() => {
      if (document.hidden && n % 5 !== 0) {
        n++
        return
      }
      void pollJobs()
      if (n % 4 === 0) void pollSlow()
      if (n % 6 === 0) void refreshFiles(false)
      n++
    }, 1500)
  }
  void boot()
}

/** Hosted: delete this sandbox's stored lists and settings (zeusd's jobs and files stay until they expire). */
export async function clearStored(): Promise<void> {
  if (HOSTED) await svc.clear()
  else for (const n of SLICES) window.localStorage.removeItem(localKey(n))
}

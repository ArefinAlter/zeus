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

import type { Approval, FileEntry, Job, JobState, Queue, Scope } from '../model/types.ts'
import { categoryOf } from '../model/categories.ts'
import { bindCode } from '../model/approvals.ts'
import { getPrefs, hydratePrefs, subscribePrefs } from '../lib/prefs.ts'
import { baseOf, joinPath, parentOf, setSep } from '../lib/paths.ts'
import { shellApi } from './shell.ts'
import type { EngineInfo, EngineSettings, NativeItem, QueuesList, RemoteApproval, RemoteArtifact, RemoteFile, RemoteJob, RemoteRun, SessionInfo } from './remote.ts'
import { RemoteError, api, backend, native, retryKey, session, svc } from './remote.ts'
import type { Settings } from './settings.ts'
import { ulid } from './rand.ts'
import type { IndexDelta, IndexRow } from './files.ts'
import { filesApi, indexFromWire, nudgeIndexSync, startIndexSync } from './files.ts'
import { historyLabel } from '../model/eventHistory.ts'
import { computeTransferProgress } from '../model/segments.ts'
import { errorOf } from '../model/downloadErrors.ts'
import { followEnqueuedDownload } from '../lib/ui.ts'

export const LIVE = backend().kind !== 'scripted'
export const HOSTED = backend().kind === 'hosted'
/** The desktop app's own engine (a bundled zeusd that the app provisions and supervises). */
export const NATIVE = backend().kind === 'native'
/** The backend keeps this client's settings and lists (the pre-alpha service, or the app's own folder). */
export const SVC = HOSTED || NATIVE

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
  /** Native: a hash the finished file must match. */
  expectedSha256?: string
  /** Native: zeusd's policy for a name that exists. */
  overwrite?: 'rename' | 'fail' | 'replace'
  /** Native: the name the person typed, as opposed to the one the server gives. */
  nameChosen?: boolean
  /** Set when sending it failed; the row shows as failed with this reason. */
  failed?: { code: string; message: string }
  sending?: boolean
}

export interface LiveInfo {
  kind: 'hosted' | 'local' | 'native'
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
  /** Native: the engine's life (starting, ready, restarting, failed) and whether settings wait for a restart. */
  engine?: EngineInfo['status'] | null
  engineDirty?: boolean
  /** Durable native history has its own status; a failed read never clears its last successful page. */
  history?: { replaying: boolean; lost: boolean; limited: boolean; unavailable: boolean }
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

/** `Documents/x.pdf` -> dir `Downloads\Documents`, name `x.pdf`. Native paths are absolute already. */
function splitPath(p: string): { dir: string; name: string } {
  if (NATIVE) return { dir: parentOf(p), name: baseOf(p) }
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
const durableHistory = new Map<string, Job['events']>()
/** Native: the queue each job belongs to and the jobs zeusd holds back for a queue that is not running. */
const queueOfJob = new Map<string, string>()
const heldJobs = new Set<string>()
let remoteFiles: RemoteFile[] = []
let indexFiles: { path: string; size: number; mtime: number; is_dir: boolean }[] = []
/** Native: the engine's index, kept current by deltas, and the ids that keep a selected row selected. */
const indexMap = new Map<string, IndexRow>()
let indexReady = false
const fileIds = new Map<string, number>()
let nextFileId = 1
let fileJobIds = new Set<string>()
const lastState = new Map<string, string>()

/**
 * The speed to show. zeusd reports its own smoothed rate; when that is still zero (the first moments of a transfer, or
 * a server that delivers in large steps) the page estimates it from how many bytes were committed over the last few
 * seconds, so a running download never reads as stalled when it is not.
 */
const rateMemo = new Map<string, { t: number; done: number; rate: number }[]>()
function shownRate(r: RemoteJob): number {
  const now = Date.now()
  const h = (rateMemo.get(r.id) ?? []).filter((x) => now - x.t < 6000)
  if (!h.length || h[h.length - 1]?.done !== r.bytes_done) h.push({ t: now, done: r.bytes_done, rate: 0 })
  rateMemo.set(r.id, h)
  if (r.rate_ewma) return r.rate_ewma
  const first = h[0]
  const last = h[h.length - 1]
  if (first && last && last.t > first.t && last.done > first.done) return Math.round(((last.done - first.done) * 1000) / (last.t - first.t))
  return 0
}

function toJob(r: RemoteJob, prev: Job | undefined): Job {
  const s = S.get()
  const meta = s.jobMeta[r.id] ?? {}
  const name = r.filename ?? baseName(r.url_redacted)
  const file = remoteFiles.find((f) => f.job_id === r.id && !f.partial) ?? remoteFiles.find((f) => f.job_id === r.id)
  const where = file ? splitPath(file.path) : NATIVE && r.path ? splitPath(r.path) : null
  const size = r.bytes_total ?? null
  // A job the queue holds back is paused in zeusd; the person sees it waiting in its queue.
  const state = (NATIVE && heldJobs.has(r.id) && r.state === 'paused' ? 'queued' : r.state) as JobState
  const art = s.live?.artifacts.find((a) => a.job_id === r.id)
  const active = ['probing', 'downloading', 'verifying', 'finalizing'].includes(state)
  const events = (NATIVE ? durableHistory.get(r.id) : prev?.events) ?? [{ t: Date.parse(r.created_at), text: 'Enqueued in zeusd' }]
  const gone = NATIVE && state === 'completed' && indexReady && !!r.path && !indexMap.has(r.path) && !fileJobIds.has(r.id)
  const job: Job = {
    id: r.id,
    name,
    url: r.url_redacted.replace('?…', ''),
    host: hostOf(r.url_redacted),
    queryWithheld: r.url_redacted.includes('?…'),
    size,
    done: r.bytes_done,
    rate: active ? shownRate(r) : 0,
    state,
    category: categoryOf(name, s.settings.categories),
    queue: NATIVE ? (queueOfJob.get(r.id) ?? null) : (meta.queue ?? null),
    added: Date.parse(r.created_at),
    ...(state === 'completed' ? { finished: Date.parse(r.updated_at) } : {}),
    dir: where?.dir ?? S.defaultDir(categoryOf(name, s.settings.categories)),
    source: meta.source ?? { kind: 'app', label: 'You, in Zeus' },
    segments: computeTransferProgress(size, r.bytes_done, active),
    connections: s.settings.downloads.connectionsPerJob,
    resumable: 'unknown',
    priority: meta.priority ?? 'normal',
    ...(gone ? { missing: true } : {}),
    ...(r.sha256 ? { sha256: r.sha256 } : {}),
    ...(art?.blake3 ? { blake3: art.blake3 } : {}),
    ...(r.quarantined ? { quarantined: true, quarantineReasons: ['zeusd quarantine'] } : {}),
    ...(state === 'failed' || state === 'retry_scheduled' ? { error: errorOf(r.last_error?.code ?? r.error_code) ?? errorOf('job.failed') } : {}),
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
      if (!NATIVE) j = { ...j, events: [...j.events.slice(-30), { t: Date.now(), text: `zeusd: ${r.state.replace('_', ' ')}` }] }
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

/** Native: the file list from the engine's index, with what zeusd knows about each file. */
function rebuildNativeFiles(): void {
  const facts = new Map<string, { job_id?: string; sha256?: string; quarantined?: boolean }>()
  for (const j of remoteJobs.values()) if (j.path) facts.set(j.path, { job_id: j.id, ...(j.sha256 ? { sha256: j.sha256 } : {}), quarantined: j.quarantined })
  for (const f of remoteFiles) {
    if (f.partial) continue
    const prev = facts.get(f.path)
    facts.set(f.path, { ...(f.job_id ?? prev?.job_id ? { job_id: (f.job_id ?? prev?.job_id) as string } : {}), ...(f.sha256 ?? prev?.sha256 ? { sha256: (f.sha256 ?? prev?.sha256) as string } : {}), quarantined: !!(f.quarantined || prev?.quarantined) })
  }
  const out: FileEntry[] = []
  for (const [path, r] of indexMap) {
    if (path.endsWith('.zpart')) continue
    let id = fileIds.get(path)
    if (id === undefined) {
      id = nextFileId++
      fileIds.set(path, id)
    }
    const f = facts.get(path)
    const url = f?.job_id ? remoteJobs.get(f.job_id)?.url_redacted : undefined
    out.push({
      id,
      name: baseOf(path),
      dir: parentOf(path),
      isFolder: r.is_dir,
      size: r.is_dir ? 0 : r.size,
      modified: r.mtime,
      ...(f?.sha256 ? { sha256: f.sha256 } : {}),
      ...(f?.job_id ? { jobId: f.job_id } : {}),
      ...(f?.quarantined ? { quarantined: true } : {}),
      ...(url ? { host: hostOf(url) } : {}),
    })
  }
  if (fileIds.size > indexMap.size * 2 + 5000) for (const p of [...fileIds.keys()]) if (!indexMap.has(p)) fileIds.delete(p)
  S.set({ files: out })
}

/** Native: apply what changed in the engine's index (or all of it) and show its state. */
export function applyIndexDelta(d: IndexDelta): void {
  let changed = false
  if (d.reset) {
    indexMap.clear()
    for (const f of d.files) indexMap.set(f.path, f)
    changed = true
  } else {
    for (const u of d.upserts) indexMap.set(u.path, u)
    for (const p of d.removes) indexMap.delete(p)
    changed = d.upserts.length > 0 || d.removes.length > 0
  }
  const firstTime = !indexReady && d.state === 'current'
  if (d.state === 'current') indexReady = true
  const s = S.get()
  const prev = s.index
  const next = { state: d.state, progress: d.progress, generation: d.generation, lastScan: d.last_scan ?? prev.lastScan, scanning: d.scanning, entries: d.entries, watcher: d.watcher, capped: d.capped, verifying: d.verifying, freshness: d.freshness, watcherGaps: d.watcher_gaps, unreadableDirectories: d.unreadable_directories, unavailableRoots: d.unavailable_roots, snapshotError: d.snapshot_error }
  if (JSON.stringify(prev) !== JSON.stringify(next)) S.set({ index: next })
  if (changed || (firstTime && s.files.length === 0)) rebuildNativeFiles()
  if (changed || firstTime) rebuildJobs()
}

function rebuildFiles(): void {
  if (NATIVE) {
    rebuildNativeFiles()
    return
  }
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
  const source = indexFiles.length || SVC ? indexFiles : remoteFiles.map((f) => ({ path: f.path, size: f.size_bytes, mtime: Date.parse(f.modified_at), is_dir: false }))
  for (const f of source) {
    if (f.is_dir) {
      addDir(f.path, f.mtime)
      continue
    }
    if (f.path.endsWith('.zpart')) continue
    if (!NATIVE) {
      const parts = f.path.split('/')
      for (let i = 1; i < parts.length; i++) addDir(parts.slice(0, i).join('/'), f.mtime)
    }
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

let jobsBusy = false
async function pollJobs(): Promise<void> {
  if (jobsBusy) return
  jobsBusy = true
  try {
    const r = await api.jobs()
    connected(r.failure)
    if (r.failure) return
    remoteJobs.clear()
    for (const j of r.items) remoteJobs.set(j.id, j)
    rebuildJobs()
  } catch (e) {
    connected(e instanceof RemoteError ? { code: e.code, message: e.message } : { code: 'client.error', message: 'The backend could not be read.' })
  } finally {
    jobsBusy = false
  }
}

let historyBusy = false
async function pollHistory(): Promise<void> {
  if (!NATIVE || historyBusy) return
  historyBusy = true
  try {
    const page = await api.history()
    const next = new Map<string, Job['events']>()
    for (const e of page.entries) {
      const text = historyLabel(e.kind, e.state)
      const t = Date.parse(e.ts)
      if (!text || !Number.isFinite(t)) continue
      const events = next.get(e.job_id) ?? []
      events.push({ t, text })
      next.set(e.job_id, events)
    }
    durableHistory.clear()
    for (const [id, events] of next) durableHistory.set(id, events)
    const live = S.get().live
    if (live) S.set({ live: { ...live, history: { replaying: page.replaying, lost: page.history_lost, limited: page.limited, unavailable: false } } })
    rebuildJobs()
  } catch {
    const live = S.get().live
    if (live) S.set({ live: { ...live, history: { replaying: false, lost: false, limited: false, ...live.history, unavailable: true } } })
  } finally {
    historyBusy = false
  }
}

let filesBusy = false
export async function refreshFiles(withIndex = false): Promise<void> {
  if (filesBusy) return
  filesBusy = true
  try {
    const [f, ix] = await Promise.all([api.files(), withIndex && SVC && !NATIVE ? svc.rescan() : Promise.resolve(null)])
    remoteFiles = f.items
    fileJobIds = new Set(f.items.map((x) => x.job_id).filter((x): x is string => !!x))
    // In the app the engine's watcher keeps the index current; a finished download only needs a quick look.
    if (NATIVE && withIndex) nudgeIndexSync()
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

// ---------------------------------------------------------------- native: queues, limiter, engine

function queueFromDaemon(q: QueuesList['queues'][number]): Queue {
  return {
    id: q.id,
    name: q.name,
    kind: q.kind,
    builtin: q.builtin,
    running: q.running,
    maxConcurrent: q.max_concurrent,
    retries: q.retries,
    schedule: {
      startEnabled: q.schedule.start_enabled,
      startTime: q.schedule.start_time,
      repeat: q.schedule.repeat,
      startDate: q.schedule.start_date,
      days: q.schedule.days as Queue['schedule']['days'],
      stopEnabled: q.schedule.stop_enabled,
      stopTime: q.schedule.stop_time,
    },
    onFinish: q.on_finish,
    limitKiBps: q.limit_kib_s,
    syncEveryHours: q.sync_every_hours ?? 24,
    syncFiles: q.sync_files ?? [],
    order: q.order,
    ...(q.last_started_ms ? { lastStarted: q.last_started_ms } : {}),
  }
}

/** The daemon's queue definition for a client queue (the shape `queues.put` takes). */
export function queueToDaemon(q: Queue): Record<string, unknown> {
  return {
    id: q.id,
    name: q.name,
    kind: q.kind,
    sync_every_hours: q.syncEveryHours,
    max_concurrent: q.maxConcurrent,
    retries: q.retries,
    retry_delay_s: 60,
    schedule: {
      start_enabled: q.schedule.startEnabled,
      start_time: q.schedule.startTime,
      repeat: q.schedule.repeat,
      start_date: q.schedule.startDate,
      days: q.schedule.days,
      stop_enabled: q.schedule.stopEnabled,
      stop_time: q.schedule.stopTime,
    },
    on_finish: q.onFinish,
    limit_kib_s: q.limitKiBps,
  }
}

let lastDefaultsKey = ''
let defaultsBusy = false
let queuesBusy = false
let lastQueues: QueuesList | null = null
let finishing = new Set<string>()
/** Read zeusd's queues, the limiter and the held jobs; carry out the on-finish actions zeusd leaves to the app. */
export async function pollQueues(): Promise<void> {
  if (!NATIVE || queuesBusy) return
  queuesBusy = true
  try {
    const r = await native.queues()
    lastQueues = r
    queueOfJob.clear()
    heldJobs.clear()
    for (const q of r.queues) {
      for (const id of q.order) queueOfJob.set(id, q.id)
      for (const id of q.held_job_ids) heldJobs.add(id)
    }
    const queues = r.queues.map(queueFromDaemon)
    const prev = S.get()
    const limiter = {
      enabled: r.limiter.enabled,
      limitKiBps: r.limiter.limit_kib_s,
      scheduleEnabled: r.limiter.schedule_enabled,
      from: r.limiter.from,
      to: r.limiter.to,
      days: r.limiter.days as typeof prev.limiter.days,
    }
    // The scheduler's defaults live in zeusd: it is the source of truth, the Settings page shows and edits them.
    const fromDaemon = { defaultQueue: r.defaults.default_queue, timezone: r.defaults.timezone, missedWindow: r.defaults.missed_window, stopBehaviour: r.defaults.stop_behaviour, connection: r.defaults.network_mode === 'metered' ? 'cellular' as const : r.defaults.network_mode === 'unmetered' ? 'lan' as const : 'auto' as const, meteredPause: r.defaults.pause_on_metered ?? true }
    const daemonKey = JSON.stringify(fromDaemon)
    let settings = prev.settings
    if (daemonKey !== lastDefaultsKey && !defaultsBusy) {
      lastDefaultsKey = daemonKey
      if (JSON.stringify({ defaultQueue: settings.scheduler.defaultQueue, timezone: settings.scheduler.timezone, missedWindow: settings.scheduler.missedWindow, stopBehaviour: settings.scheduler.stopBehaviour, connection: settings.connection.type, meteredPause: settings.connection.meteredPause }) !== daemonKey) {
        settings = { ...settings, scheduler: { ...settings.scheduler, defaultQueue: fromDaemon.defaultQueue, timezone: fromDaemon.timezone, missedWindow: fromDaemon.missedWindow, stopBehaviour: fromDaemon.stopBehaviour }, connection: { ...settings.connection, type: fromDaemon.connection, meteredPause: fromDaemon.meteredPause } }
      }
    }
    settings = { ...settings, connection: { ...settings.connection, cost: r.network?.cost ?? 'unknown', policyHeld: r.network?.held_jobs ?? 0 } }
    S.set({ queues, limiter, settings, killSwitch: r.kill_switch_engaged || prev.killSwitch })
    rebuildJobs()
    for (const q of r.queues) {
      if (!q.finish_action_pending) {
        finishing.delete(q.id)
        continue
      }
      if (finishing.has(q.id)) continue
      finishing.add(q.id)
      void finishQueue(q)
    }
  } catch {
    // The job poll shows the connection state.
  } finally {
    queuesBusy = false
  }
}

async function finishQueue(q: QueuesList['queues'][number]): Promise<void> {
  const text =
    q.on_finish === 'notify'
      ? 'Notified you'
      : q.on_finish === 'stop_limiter'
        ? 'Turned the speed limiter off'
        : q.on_finish === 'close_app'
          ? 'Closing Zeus'
          : q.on_finish === 'sleep'
            ? 'Putting the computer to sleep'
            : 'Nothing else to do'
  S.toast({ kind: 'ok', title: `${q.name} finished`, desc: text, important: true, topic: 'queue' })
  try {
    await native.queueOp('control', { action: 'ack_finish', id: q.id })
  } catch {
    // Acknowledged on the next poll.
  }
  if (q.on_finish === 'close_app' || q.on_finish === 'sleep') {
    try {
      await native.finishAction(q.on_finish)
    } catch {
      S.toast({ kind: 'warn', title: 'Could not do that', desc: `${q.name} finished, but the app could not ${q.on_finish === 'sleep' ? 'put the computer to sleep' : 'close'}.`, important: false, topic: 'queue' })
    }
  }
}

/** Apply a client-side queue change in zeusd (put) and refresh. */
export async function putQueue(q: Queue): Promise<void> {
  await native.queueOp('put', { queue: queueToDaemon(q) })
  await pollQueues()
}

export async function queueControl(body: Record<string, unknown>): Promise<void> {
  await native.queueOp('control', body)
  await pollQueues()
}

export async function deleteQueueRemote(id: string): Promise<void> {
  await native.queueOp('delete', { id })
  await pollQueues()
}

export async function configureLimiter(limiter: { enabled: boolean; limitKiBps: number; scheduleEnabled: boolean; from: string; to: string; days: number[] }): Promise<void> {
  await native.queueOp('configure', { limiter: { enabled: limiter.enabled, limit_kib_s: limiter.limitKiBps, schedule_enabled: limiter.scheduleEnabled, from: limiter.from, to: limiter.to, days: limiter.days } })
  await pollQueues()
}

export async function configureDefaults(d: { timezone: 'local' | 'utc'; missedWindow: 'skip' | 'run_once'; stopBehaviour: 'pause' | 'finish_current'; defaultQueue: string }): Promise<void> {
  await native.queueOp('configure', { defaults: { timezone: d.timezone, missed_window: d.missedWindow, stop_behaviour: d.stopBehaviour, default_queue: d.defaultQueue, network_mode: S.get().settings.connection.type === 'cellular' ? 'metered' : S.get().settings.connection.type === 'lan' ? 'unmetered' : 'auto', pause_on_metered: S.get().settings.connection.meteredPause } })
  await pollQueues()
}

export function daemonQueues(): QueuesList | null {
  return lastQueues
}

/** The weekly check for a newer release, when the person turned it on: once a week at most, remembered on this device. */
async function weeklyUpdateCheck(): Promise<void> {
  const KEY = 'zeus.update.lastcheck.v1'
  try {
    const last = Number(window.localStorage.getItem(KEY) ?? 0)
    if (Date.now() - last < 7 * 86_400_000) return
    window.localStorage.setItem(KEY, String(Date.now()))
  } catch {
    // Without storage the check runs once per start.
  }
  try {
    const u = await shellApi.update()
    if (u.newer) S.toast({ kind: '', title: `A newer Zeus is out: ${u.latest}`, desc: u.url ?? 'See the releases page on GitHub.', important: true, topic: 'system' })
  } catch {
    // Quiet: the person can check by hand in Settings.
  }
}

let engineBusy = false
async function pollEngine(): Promise<void> {
  if (!NATIVE || engineBusy) return
  engineBusy = true
  try {
    const e = await native.engine()
    const s = S.get()
    if (s.live) S.set({ live: { ...s.live, engine: e.status } })
  } catch {
    // The job poll shows the connection state.
  } finally {
    engineBusy = false
  }
}

/** The engine settings that live in zeusd's config file, from the client's Settings sections. */
export function engineSettingsFrom(settings: Settings): EngineSettings {
  return {
    write_roots: settings.saveTo.writeRoots,
    concurrent_jobs: settings.downloads.concurrentJobs,
    connections_per_job: settings.downloads.connectionsPerJob,
    connections_per_host: settings.downloads.connectionsPerHost,
    connect_s: settings.downloads.connectS,
    read_s: settings.downloads.readS,
    probe_s: settings.downloads.probeS,
    tombstone_days: settings.downloads.tombstoneDays,
    proxy: settings.connection.proxy === 'http' ? 'http' : 'system',
    proxy_host: settings.connection.proxyHost,
    proxy_port: settings.connection.proxyPort,
    proxy_bypass: settings.connection.proxyBypass,
  }
}

let appliedEngine = ''
function engineKey(e: EngineSettings): string {
  return JSON.stringify(e)
}

/** Save the engine settings to zeusd's config and restart the engine (the person asked for it). */
export async function applyEngineSettings(): Promise<void> {
  const settings = S.get().settings
  const e = engineSettingsFrom(settings)
  await native.putEngineSettings(e)
  appliedEngine = engineKey(e)
  await native.restart()
  const s = S.get()
  if (s.live) S.set({ live: { ...s.live, engineDirty: false } })
}

function watchEngineSettings(): void {
  S.subscribe(() => {
    const s = S.get()
    if (!s.live?.loaded || !s.live) return
    // A change of the scheduler's defaults goes to zeusd at once.
    const sc = s.settings.scheduler
    const wanted = JSON.stringify({ defaultQueue: sc.defaultQueue, timezone: sc.timezone, missedWindow: sc.missedWindow, stopBehaviour: sc.stopBehaviour, connection: s.settings.connection.type, meteredPause: s.settings.connection.meteredPause })
    if (lastDefaultsKey && wanted !== lastDefaultsKey && !defaultsBusy) {
      defaultsBusy = true
      void configureDefaults({ timezone: sc.timezone, missedWindow: sc.missedWindow, stopBehaviour: sc.stopBehaviour, defaultQueue: sc.defaultQueue })
        .then(() => {
          lastDefaultsKey = wanted
        })
        .catch(() => undefined)
        .finally(() => {
          defaultsBusy = false
        })
    }
    const dirty = engineKey(engineSettingsFrom(s.settings)) !== appliedEngine
    if (dirty !== !!s.live.engineDirty) S.set({ live: { ...s.live, engineDirty: dirty } })
  })
}

async function pollSlow(): Promise<void> {
  try {
    const [st, arts, aps, runs] = await Promise.all([api.status(), api.artifacts(), api.approvals(), api.runs()])
    const s = S.get()
    if (s.live) S.set({ live: { ...s.live, daemon: st.daemon, artifacts: arts.items, runs: runs.items } })
    daemonApprovals = aps.items.map(approvalFromDaemon)
    if (SVC) {
      const held = await svc.approvals()
      serviceApprovals = (held.items as Approval[]).map((a) => ({ ...a, heldBy: 'service' as const }))
    }
    mergeApprovals()
  } catch {
    // Shown by the job poll's connection state.
  }
}

// ---------------------------------------------------------------- persistence of what zeusd does not hold

const ALL_SLICES = ['queues', 'limiter', 'settings', 'skills', 'threads', 'bookmarks', 'history', 'connectors', 'projects', 'grabber', 'jobmeta', 'pending', 'killswitch'] as const
// The daemon owns queues and the limiter in the app's own engine.
const SLICES = ALL_SLICES.filter((n) => !(NATIVE && (n === 'queues' || n === 'limiter')))
type Slice = (typeof ALL_SLICES)[number]

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

// Serialize writes per slice: a slow older request must not overwrite newer state.
const saveQueues = new Map<string, Promise<void>>()
function saveSlice(name: string, value: unknown): Promise<void> {
  const previous = saveQueues.get(name) ?? Promise.resolve()
  const next = previous.catch(() => undefined).then(async () => {
    if (SVC) await svc.put(name, value)
    else window.localStorage.setItem(localKey(name), JSON.stringify(value))
  })
  saveQueues.set(name, next)
  void next.finally(() => { if (saveQueues.get(name) === next) saveQueues.delete(name) }).catch(() => undefined)
  return next
}

/** Persist a review before its controls are exposed; failures must remain visible. */
export async function persistAgentReview(): Promise<void> {
  dirty.delete('threads')
  await saveSlice('threads', sliceValue('threads'))
}

function flush(): void {
  saveTimer = undefined
  const names = [...dirty]
  dirty.clear()
  for (const name of names) {
    const value = name === 'prefs' ? getPrefs() : sliceValue(name as Slice)
    void saveSlice(name, value).catch((e: unknown) => {
      if (e instanceof RemoteError) S.toast({ kind: 'warn', title: 'Not saved', desc: e.message, important: true, topic: 'system' })
    })
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
  if (SVC)
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
  if (SVC) {
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

/** Native: one preview of the whole batch (each item carries its folder, queue and options), then that batch once. */
async function sendNative(items: PendingItem[]): Promise<void> {
  const fail = (code: string, message: string, which: PendingItem[] = items) => {
    patchPending(which.map((p) => p.id), (p) => {
      const { sending: _s, ...rest } = p
      return { ...rest, failed: { code, message } }
    })
    S.toast({ kind: 'bad', title: 'Not started', desc: message, important: true, topic: 'failure' })
  }
  try {
    const batch: NativeItem[] = items.map((p) => ({
      url: p.url,
      dir: p.dir,
      ...(p.nameChosen ? { filename: p.name } : {}),
      ...(p.expectedSha256 ? { expected_sha256: p.expectedSha256 } : {}),
      ...(p.queue ? { queue: p.queue } : {}),
      overwrite: p.overwrite === 'replace' ? 'rename' : (p.overwrite ?? ({ rename: 'rename', fail: 'fail', replace: 'rename' } as const)[S.get().settings.downloads.duplicates]),
      max_connections: S.get().settings.downloads.connectionsPerJob,
    }))
    const pv = await api.previewItems(batch)
    if (pv.refused.length) {
      const bad = new Set(pv.refused.map((r) => r.index))
      for (const r of pv.refused) {
        const p = items[r.index]
        if (p) fail('policy.refused', r.reason, [p])
      }
      const ok = items.filter((_, i) => !bad.has(i))
      patchPending(ok.map((p) => p.id), (p) => {
        const { sending: _s, ...rest } = p
        return rest
      })
      if (ok.length) void send(ok.map((p) => p.id))
      return
    }
    if (!pv.ready || !pv.review_id) {
      const first = pv.items.find((i) => i.error_code)
      return fail(first?.error_code ?? 'job.preview_failed', first?.error_message ?? 'zeusd could not plan these downloads. Check the addresses and folders.')
    }
    const r = await api.enqueue(pv.review_id, retryKey())
    const meta = { ...S.get().jobMeta }
    r.jobs.forEach((j, i) => {
      const p = items[i]
      if (!j.job_id || !p) return
      meta[j.job_id] = { queue: p.queue, source: p.source, ...(p.project ? { project: p.project } : {}), ...(p.description ? { description: p.description } : {}) }
    })
    const started = new Set(items.filter((_, i) => r.jobs[i]?.job_id).map((p) => p.id))
    S.set({ jobMeta: meta, pending: S.get().pending.filter((p) => !started.has(p.id)) })
    const failedHere = items.filter((p) => !started.has(p.id))
    if (failedHere.length) fail(r.jobs.find((j) => j.error_code)?.error_code ?? 'job.enqueue_failed', r.jobs.find((j) => j.error_message)?.error_message ?? 'zeusd refused some of these downloads.', failedHere)
    await Promise.all([pollJobs(), pollQueues()])
    r.jobs.forEach((j, i) => {
      if (j.job_id && items[i]) followEnqueuedDownload(items[i].id, j.job_id)
    })
  } catch (e) {
    fail(e instanceof RemoteError ? e.code : 'client.error', e instanceof RemoteError ? e.message : 'The download could not be sent to zeusd.')
  }
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
  if (NATIVE) return sendNative(items)
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
      r.jobs.forEach((j, i) => {
        if (j.job_id && group[i]) followEnqueuedDownload(group[i].id, j.job_id)
      })
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
  const kind = backend().kind === 'local' ? 'local' : NATIVE ? 'native' : 'hosted'
  S.set({ live: { kind, connected: false, loaded: false, failure: null, session: null, daemon: null, runs: [], artifacts: [], lastJobs: 0, engine: null, engineDirty: false } })
  watchSlices()
  const boot = async () => {
    try {
      const sess = await session()
      if (NATIVE) setSep(sess.sep)
      const s = S.get()
      const root = sess.root_label
      const roots = NATIVE && sess.roots?.length ? sess.roots : [root]
      S.set({
        live: { ...(s.live as LiveInfo), session: sess, connected: true, failure: null },
        settings: {
          ...s.settings,
          saveTo: { ...s.settings.saveTo, root, tempDir: NATIVE ? `${joinPath(root, '.zeus-partial')}` : `${root}\\.zeus-partial`, writeRoots: roots },
          index: { ...s.settings.index, roots: roots.map((path) => ({ path, kind: 'folder' as const, include: true, monitorChanges: NATIVE, rescanEveryHours: null })) },
        },
      })
      try {
        await loadSlices()
      } catch {
        S.toast({ kind: 'warn', title: 'Settings not loaded', desc: 'The pre-alpha service did not answer; changes are kept for this window.', important: false, topic: 'system' })
      }
      // The sandbox's root is the server's, whatever was stored. In the app the engine's own settings win.
      const cur = S.get()
      let engineSettings: EngineSettings | null = null
      if (NATIVE) {
        try {
          engineSettings = await native.engineSettings()
        } catch {
          engineSettings = null
        }
      }
      const nativeRoots = engineSettings?.write_roots?.length ? engineSettings.write_roots : roots
      S.set({
        settings: {
          ...cur.settings,
          saveTo: { ...cur.settings.saveTo, root: NATIVE ? (nativeRoots[0] ?? root) : root, writeRoots: NATIVE ? nativeRoots : [root] },
          ...(engineSettings
            ? {
                downloads: {
                  ...cur.settings.downloads,
                  concurrentJobs: engineSettings.concurrent_jobs,
                  connectionsPerJob: engineSettings.connections_per_job,
                  connectionsPerHost: engineSettings.connections_per_host,
                  connectS: engineSettings.connect_s,
                  readS: engineSettings.read_s,
                  probeS: engineSettings.probe_s,
                  tombstoneDays: engineSettings.tombstone_days,
                },
                connection: { ...cur.settings.connection, proxy: engineSettings.proxy, proxyHost: engineSettings.proxy_host, proxyPort: engineSettings.proxy_port, proxyBypass: engineSettings.proxy_bypass },
              }
            : {}),
          index: { ...cur.settings.index, roots: (NATIVE ? nativeRoots : [root]).map((path) => ({ path, kind: 'folder' as const, include: true, monitorChanges: NATIVE, rescanEveryHours: null })) },
        },
        live: { ...(cur.live as LiveInfo), loaded: true },
      })
      if (NATIVE) {
        appliedEngine = engineKey(engineSettingsFrom(S.get().settings))
        watchEngineSettings()
        void shellApi.get().then(
          (v) => {
            const st = S.get()
            S.set({ settings: { ...st.settings, general: { ...st.settings.general, launchAtLogin: v.settings.launch_at_login, startMinimized: v.settings.start_minimized, showTray: v.settings.show_tray, closeToTray: v.settings.close_to_tray, checkUpdates: v.settings.check_updates_weekly ? 'weekly' : 'never' } } })
            if (v.settings.check_updates_weekly) void weeklyUpdateCheck()
          },
          () => undefined,
        )
        // The index's own configuration (folders, exclusions) lives in the engine's app folder and wins over a stored copy.
        try {
          const wire = await filesApi.config()
          const here = S.get()
          S.set({ settings: { ...here.settings, index: indexFromWire(wire, here.settings.index) } })
        } catch {
          // The Settings page asks again.
        }
      }
      for (const n of SLICES) saved.set(n, sliceRef(n))
      if (NATIVE) {
        startIndexSync({ apply: applyIndexDelta, hidden: () => document.hidden })
      } else if (SVC) {
        try {
          const ix = await svc.index()
          indexFiles = ix.files
          S.set({ index: { state: 'current', progress: 1, generation: ix.generation, lastScan: ix.last_scan ?? Date.now() } })
        } catch {
          // Built from zeusd's file list instead.
        }
      }
      await Promise.all([pollJobs(), pollHistory(), refreshFiles(false), pollSlow(), pollQueues(), pollEngine()])
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
      void pollHistory()
      if (NATIVE && n % 2 === 0) void pollQueues()
      if (NATIVE && n % 3 === 0) void pollEngine()
      if (n % 4 === 0) void pollSlow()
      if (n % 6 === 0) void refreshFiles(false)
      n++
    }, 1500)
  }
  void boot()
}

/** Hosted: delete this sandbox's stored lists and settings (zeusd's jobs and files stay until they expire). */
export async function clearStored(): Promise<void> {
  if (SVC) await svc.clear()
  else for (const n of SLICES) window.localStorage.removeItem(localKey(n))
}

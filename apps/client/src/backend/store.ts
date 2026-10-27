// The scripted backend. It stands in for zeusd in this pre-alpha release: it holds the jobs, queues, approvals, the
// file index and the agent threads in memory, advances transfers twice a second, starts a queue on its schedule,
// fails one download on an expired link and lets another wait for a retry, and raises an approval that waits for a
// person. It makes no network request and writes nothing to disk. Reloading starts it again.
//
// The rules it follows are the real ones where Zeus has them: the job states and their transitions (spec 01), the
// approval facts, request hash and bind code (spec 07), the 50-link review limit (apps/desktop), and the closed tool
// table of the agent. Nothing an agent thread does can reach `decide`.

import { useEffect, useRef, useState } from 'preact/hooks'

import { DEFAULT_INSTRUCTIONS, TOOLS } from '../model/agentTools.ts'
import { ARMING_MS, allowedScopes, bindCode, confirmationFor, requestHash } from '../model/approvals.ts'
import { categoryOf, extOf } from '../model/categories.ts'
import { intake, nameProblem } from '../model/links.ts'
import { jobRateCap, startDue, stopDue } from '../model/schedule.ts'
import type { Approval, ApprovalFacts, ApprovalKind, CategoryId, FileEntry, Job, JobSource, JobState, Queue, Scope, SpeedLimiter } from '../model/types.ts'
import { fmtBytes, setTimeFormat } from '../model/units.ts'
import { CONNECTORS } from './connectors.ts'
import type { JobMeta, LiveInfo, PendingItem } from './live.ts'
import { HOSTED, LIVE, NATIVE, SVC, clearStored, configureLimiter, control as liveControl, deleteQueueRemote, hostRefusal, newPendingId, putQueue, queueControl, rebuildJobs, refreshFiles, send as liveSend, startLive, subdirOf } from './live.ts'
import type { SitePreview } from './sitegrab.ts'
import { api, retryKey, svc } from './remote.ts'
import { ulid } from './rand.ts'
import { filesApi, nudgeIndexSync } from './files.ts'
import { isInside, joinPath, SEP } from '../lib/paths.ts'
import type { DupeChoice, Existing, Overwrite } from '../model/grab.ts'
import { checkNote, duplicateWhere, findDuplicate, folderFor, overwriteFor } from '../model/grab.ts'
import { ARCHIVE, CLAUDE, EDGE, ROOT, UNITS, YOU, fileOfJob, makeJob, makeSegments, sampleIndex, sampleJobs, sampleLimiter, sampleQueues } from './sample.ts'
import type { Settings } from './settings.ts'
import { defaultSettings } from './settings.ts'
import type { SiteRequest } from './sitegrab.ts'
import { previewSite } from './sitegrab.ts'
import type { Skill } from './skills.ts'
import { defaultSkills } from './skills.ts'
import type { Thread } from './threads.ts'
import { sampleThreads } from './threads.ts'

const { MiB, MIN, HOUR, DAY } = UNITS
void isInside
const TICK_MS = 500

export interface Toast {
  id: number
  kind: 'ok' | 'bad' | 'warn' | ''
  title: string
  desc?: string
  important: boolean
  /** Which notification setting governs it. */
  topic: 'complete' | 'failure' | 'approval' | 'queue' | 'system'
  to?: string
  sound?: 'done' | 'fail'
}

export interface GrabberLink {
  id: string
  url: string
  display: string
  host: string
  queryWithheld: boolean
  name: string
  check: 'unchecked' | 'checking' | 'online' | 'offline' | 'needs_login'
  size: number | null
  type: string
  category: CategoryId
  dir: string
  selected: boolean
  /** Package name; defaults to the host. */
  pkg: string
  /** What the online check learned, in a sentence for the row. */
  note?: string
  /** HTTP status of the online check. */
  status?: number
  resumable?: 'verified' | 'unsupported' | 'unknown' | null
  /** The hash the publisher gives; zeusd refuses the file if the bytes differ. */
  expectedSha256?: string
  /** The folder was typed by the person: a category or name change leaves it alone. */
  dirEdited?: boolean
  /** The same file is already downloaded, queued or running. */
  dupe?: { id: string; state: string; name: string; where: string }
  dupeChoice?: DupeChoice
}

/** One row of zeusd's dry run for a reviewed batch. */
export interface PlanRow {
  index: number
  filename?: string | null
  destination?: string | null
  warnings?: string[] | null
  size?: number | null
  resumable?: string | null
  approval_needed?: boolean
  error_code?: string | null
  error_message?: string | null
}

export interface Review {
  id: string
  created: number
  links: GrabberLink[]
  /** Exactly what would be created: destination paths after collision renaming. */
  destinations: { name: string; dir: string; collides: boolean }[]
  /** Live backend: zeusd's dry-run of this batch, one per destination folder. */
  remote?: { state: 'checking' | 'ready' | 'refused'; reason?: string; reviews: { reviewId: string; linkIds: string[] }[]; plan?: PlanRow[] }
  /** The overwrite policy chosen for the batch (a duplicate's own choice wins for its row). */
  overwrite?: Overwrite
  /** Links left out of the review: duplicates the person chose to skip. */
  skippedDuplicates?: number
}

export interface SiteProject {
  id: string
  name: string
  startUrl: string
  request: SiteRequest
  /** Live backend: the files the walk over the sample site found, started after approval. */
  found?: { url: string; name: string; size: number }[]
  created: number
  state: 'awaiting_approval' | 'running' | 'denied' | 'finished'
  files: number
  bytes: number
  approvalId?: string
  queue: string | null
}

export interface Bookmark {
  id: string
  name: string
  query: string
  filter: string
}

export interface IndexState {
  state: 'current' | 'scanning'
  progress: number
  generation: number
  lastScan: number
  scanning?: string
  /** Native: entries in the engine's index, whether the watcher follows changes, the cap, the start-up check. */
  entries?: number
  watcher?: boolean
  capped?: boolean
  verifying?: boolean
}

export interface State {
  now: number
  t0: number
  jobs: Job[]
  queues: Queue[]
  limiter: SpeedLimiter
  approvals: Approval[]
  files: FileEntry[]
  index: IndexState
  bookmarks: Bookmark[]
  history: string[]
  threads: Thread[]
  skills: Skill[]
  settings: Settings
  grabber: GrabberLink[]
  reviews: Review[]
  projects: SiteProject[]
  toasts: Toast[]
  killSwitch: boolean
  connectorsAdded: string[]
  /** Live backend only: display facts about zeusd jobs, downloads waiting in queues, and the connection. */
  jobMeta: Record<string, JobMeta>
  pending: PendingItem[]
  live: LiveInfo | null
}

// ---------------------------------------------------------------- the store

let state: State = init()
const listeners = new Set<() => void>()
let toastSeq = 1

function init(): State {
  const now = Date.now()
  const jobs = sampleJobs(now)
  const queues = sampleQueues(now)
  for (const q of queues) q.order = jobs.filter((j) => j.queue === q.id).map((j) => j.id)
  const settings = defaultSettings()
  settings.agent.toolsEnabled = TOOLS.map((t) => t.name)
  settings.agent.instructions = DEFAULT_INSTRUCTIONS
  const s: State = {
    now,
    t0: now,
    jobs,
    queues,
    limiter: sampleLimiter(),
    approvals: [],
    files: sampleIndex(now, jobs),
    index: { state: 'current', progress: 1, generation: 14, lastScan: now - 3 * HOUR },
    bookmarks: [
      { id: 'b1', name: 'Large videos', query: 'size:>1gb', filter: 'video' },
      { id: 'b2', name: 'This week\'s documents', query: 'dm:thisweek', filter: 'document' },
      { id: 'b3', name: 'Installers to check', query: 'ext:exe;msi', filter: 'everything' },
    ],
    history: ['report 2025', 'ext:flac', 'lecture', 'size:huge'],
    threads: sampleThreads(now),
    skills: defaultSkills(),
    settings,
    grabber: [],
    reviews: [],
    projects: [
      {
        id: 'proj_docs',
        name: 'docs.example.org',
        startUrl: 'https://docs.example.org/',
        request: { startUrl: 'https://docs.example.org/', depth: 2, sameHostOnly: true, types: ['pdf', 'zip', 'md'], include: [], exclude: ['*/blog/*'], maxFiles: 50, maxMiB: 500 },
        created: now - 8 * DAY,
        state: 'finished',
        files: 4,
        bytes: 54 * MiB,
        queue: null,
      },
    ],
    toasts: [],
    killSwitch: false,
    connectorsAdded: ['google-drive'],
    jobMeta: {},
    pending: [],
    live: null,
  }
  if (LIVE) {
    // A real backend starts from nothing but the defaults: no sample jobs, files, requests or projects.
    s.jobs = []
    s.files = []
    s.projects = []
    s.approvals = []
    s.connectorsAdded = []
    s.index = { state: 'current', progress: 1, generation: 0, lastScan: now }
    s.history = []
    for (const q of s.queues) q.order = []
    if (NATIVE) {
      s.queues = []
      s.bookmarks = []
      s.threads = []
      s.settings.siteLogins = []
      s.settings.browser = { ...s.settings.browser, extension: 'not_installed', browsers: [] }
    }
    return s
  }
  s.approvals = sampleApprovals(now, s)
  return s
}

export function getState(): State {
  return state
}

function emit(): void {
  for (const l of listeners) l()
}

function set(patch: Partial<State>): void {
  state = { ...state, ...patch }
  emit()
}

/** For the live adapter only. */
export function setState(patch: Partial<State>): void {
  set(patch)
}

function meta(ids: string[], f: (m: JobMeta) => JobMeta): void {
  const next = { ...state.jobMeta }
  for (const id of ids) next[id] = f(next[id] ?? {})
  set({ jobMeta: next })
}

const isPending = (id: string) => id.startsWith('pend_')

/** The download folder as shown: the sandbox's (live) or the sample one. */
const rootDir = () => (LIVE ? state.settings.saveTo.root : ROOT)

export function subscribe(f: () => void): () => void {
  listeners.add(f)
  return () => {
    listeners.delete(f)
  }
}

/** A value from the store; the component re-renders when the selected value changes (by identity). */
export function useStore<T>(select: (s: State) => T): T {
  const [value, setValue] = useState(() => select(state))
  const sel = useRef(select)
  sel.current = select
  useEffect(() => {
    const on = () => setValue(() => sel.current(state))
    on()
    return subscribe(on)
  }, [])
  return value
}

export function toast(t: Omit<Toast, 'id'>): void {
  const id = toastSeq++
  set({ toasts: [...state.toasts.slice(-3), { ...t, id }] })
}

export function dismissToast(id: number): void {
  set({ toasts: state.toasts.filter((t) => t.id !== id) })
}

function patchJob(id: string, f: (j: Job) => Job): void {
  set({ jobs: state.jobs.map((j) => (j.id === id ? f(j) : j)) })
}

function note(j: Job, text: string, t = Date.now()): Job {
  return { ...j, events: [...j.events.slice(-30), { t, text }] }
}

export function jobById(id: string): Job | undefined {
  return state.jobs.find((j) => j.id === id)
}

// ---------------------------------------------------------------- jobs

const ACTIVE: JobState[] = ['probing', 'downloading', 'verifying', 'finalizing']

export function isActive(j: Job): boolean {
  return ACTIVE.includes(j.state)
}

function stopSegments(j: Job): Job {
  return { ...j, rate: 0, segments: j.segments.map((s) => (s.state === 'active' ? { ...s, state: 'pending' } : s)) }
}

/** Pause: workers stop, every committed byte stays (spec 01 rule 8). Verifying and finalizing cannot be paused. */
export function pause(ids: string[]): number {
  if (LIVE) {
    const real = state.jobs.filter((j) => ids.includes(j.id) && !isPending(j.id) && ['queued', 'probing', 'downloading', 'retry_scheduled', 'awaiting_session'].includes(j.state))
    void liveControl(real.map((j) => j.id), 'pause')
    return real.length
  }
  let n = 0
  set({
    jobs: state.jobs.map((j) => {
      if (!ids.includes(j.id) || !['queued', 'probing', 'downloading', 'retry_scheduled', 'awaiting_session'].includes(j.state)) return j
      n++
      return note(stopSegments({ ...j, state: 'paused' }), 'Paused by you; saved bytes kept')
    }),
  })
  return n
}

/** Resume a paused job, retry a failed one from its saved bytes, or start a queued one now. */
export function resume(ids: string[]): number {
  if (state.killSwitch) {
    toast({ kind: 'warn', title: 'Downloads are stopped', desc: 'Release the global stop first. Releasing restores no permission and no work by itself.', important: true, topic: 'system' })
    return 0
  }
  if (LIVE) {
    const chosen = state.jobs.filter((j) => ids.includes(j.id))
    const pend = chosen.filter((j) => isPending(j.id)).map((j) => j.id)
    const paused = chosen.filter((j) => !isPending(j.id) && j.state === 'paused').map((j) => j.id)
    const failed = chosen.filter((j) => !isPending(j.id) && j.state === 'failed').map((j) => j.id)
    if (pend.length) void liveSend(pend)
    if (paused.length) void liveControl(paused, 'resume')
    if (failed.length) void liveControl(failed, 'retry')
    return pend.length + paused.length + failed.length
  }
  let n = 0
  set({
    jobs: state.jobs.map((j) => {
      if (!ids.includes(j.id)) return j
      if (j.state === 'paused' || j.state === 'queued' || j.state === 'failed' || j.state === 'retry_scheduled') {
        n++
        const { error: _e, retryAt: _r, ...rest } = j
        return note({ ...rest, state: 'probing', probeUntil: Date.now() + 900, queue: j.state === 'queued' ? null : j.queue }, j.state === 'failed' ? `Retried by you from ${fmtBytes(j.done)} saved` : j.state === 'queued' ? 'Started by you' : 'Resumed by you')
      }
      return j
    }),
  })
  return n
}

export function pauseAll(): number {
  return pause(state.jobs.filter((j) => isActive(j) || j.state === 'queued' || j.state === 'retry_scheduled').map((j) => j.id))
}

export function resumeAll(): number {
  return resume(state.jobs.filter((j) => j.state === 'paused').map((j) => j.id))
}

/** Remove from the list. A finished file stays on disk unless `deleteFile`; partial data of an unfinished job is deleted (cancel). */
export function remove(ids: string[], deleteFile: boolean): number {
  if (LIVE) {
    const real = ids.filter((id) => !isPending(id))
    // zeusd keeps its job records; this client only hides them. Files stay: the gui credential cannot delete files,
    // and the hosted sandbox removes them after 24 hours.
    meta(real, (m) => ({ ...m, hidden: true }))
    set({ pending: state.pending.filter((p) => !ids.includes(p.id)), queues: state.queues.map((q) => ({ ...q, order: q.order.filter((x) => !ids.includes(x)) })) })
    if (NATIVE && deleteFile && real.length) {
      const paths = state.files.filter((f) => f.jobId && real.includes(f.jobId) && !f.isFolder).map((f) => joinPath(f.dir, f.name))
      if (paths.length) {
        void filesApi.remove(paths, false).then(
          (r) => {
            toast({ kind: r.failed.length ? 'warn' : 'ok', title: r.failed.length ? 'Some files were not deleted' : 'Files moved to the Recycle Bin', desc: r.failed[0]?.message ?? `${r.trashed.length + r.deleted.length} file${paths.length === 1 ? '' : 's'}`, important: !!r.failed.length, topic: 'system' })
            nudgeIndexSync()
          },
          (e: unknown) => toast({ kind: 'warn', title: 'Files kept', desc: e instanceof Error ? e.message : 'The files could not be deleted.', important: true, topic: 'system' }),
        )
      }
    } else if (deleteFile && real.length) toast({ kind: 'warn', title: 'Files kept', desc: 'This client cannot delete files: its restricted zeusd credential has no file write permission. Hosted files expire after 24 hours.', important: true, topic: 'system' })
    rebuildJobs()
    return ids.length
  }
  const gone = state.jobs.filter((j) => ids.includes(j.id))
  const files = deleteFile ? state.files.filter((f) => !f.jobId || !ids.includes(f.jobId)) : state.files
  set({
    jobs: state.jobs.filter((j) => !ids.includes(j.id)),
    files,
    queues: state.queues.map((q) => ({ ...q, order: q.order.filter((x) => !ids.includes(x)) })),
  })
  return gone.length
}

export function cancel(ids: string[]): void {
  if (LIVE) {
    set({ pending: state.pending.filter((p) => !ids.includes(p.id)) })
    const real = state.jobs.filter((j) => ids.includes(j.id) && !isPending(j.id) && !['completed', 'canceled', 'finalizing'].includes(j.state)).map((j) => j.id)
    if (real.length) void liveControl(real, 'cancel')
    else rebuildJobs()
    return
  }
  set({
    jobs: state.jobs.map((j) => (ids.includes(j.id) && !['completed', 'canceled', 'finalizing'].includes(j.state) ? note({ ...stopSegments(j), state: 'canceled', done: 0, segments: makeSegments(j.size, j.connections, 0, false) }, 'Canceled; partial data deleted') : j)),
  })
}

export function deleteCompleted(): number {
  return remove(state.jobs.filter((j) => j.state === 'completed').map((j) => j.id), false)
}

export function redownload(id: string): void {
  const j = jobById(id)
  if (!j) return
  if (LIVE) {
    if (j.queryWithheld) {
      toast({ kind: 'warn', title: 'Add the link again', desc: "Zeus never shows a link's query string, so it cannot repeat this one. Paste the full link into Add URL.", important: false, topic: 'system' })
      return
    }
    addDownloads([{ url: j.url, queue: null, start: true }])
    return
  }
  patchJob(id, (x) => {
    const { sha256: _a, blake3: _b, finished: _f, matchedExpected: _m, quarantined: _q, quarantineReasons: _r, error: _e, ...rest } = x
    return note({ ...rest, state: 'probing', probeUntil: Date.now() + 900, done: 0, segments: makeSegments(x.size, x.connections, 0, true) }, 'Download again: the existing file is kept and the new one is renamed if it collides')
  })
}

/** Refresh the address of a job whose signed link expired: the new address replaces the old one; saved bytes stay valid only if the server's validators match. */
export function refreshAddress(id: string, url?: string): string | null {
  const j = jobById(id)
  if (!j) return 'No such job.'
  if (LIVE) {
    if (!url) return 'Asking the browser for a fresh address needs the Zeus extension, which this client does not use. Paste a fresh link instead.'
    const r = addDownloads([{ url, name: j.name, dir: j.dir, queue: null, start: true }])
    return r.refused[0] ?? null
  }
  let host = j.host
  let display = j.url
  let withheld = j.queryWithheld
  if (url) {
    let u: URL
    try {
      u = new URL(url)
    } catch {
      return 'Not an address.'
    }
    if (u.protocol !== 'https:' && u.protocol !== 'http:') return 'Zeus downloads http and https only.'
    if (u.username || u.password) return 'No user name or password in the address.'
    host = u.hostname
    display = `${u.protocol}//${u.host}${u.pathname}`
    withheld = u.search.length > 1
  }
  patchJob(id, (x) => {
    const { error: _e, ...rest } = x
    return note({ ...rest, url: display, host, queryWithheld: withheld, state: x.state === 'failed' ? 'probing' : x.state, probeUntil: Date.now() + 900 }, url ? 'Address refreshed by you; validators matched, saved bytes kept' : 'Asked the browser for a fresh address; validators matched, saved bytes kept')
  })
  return null
}

function failToast(title: string) {
  return (e: unknown) => toast({ kind: 'warn', title, desc: e instanceof Error ? e.message : 'zeusd did not accept that.', important: false, topic: 'queue' })
}

export function setQueue(ids: string[], queue: string | null): void {
  if (NATIVE) {
    const real = ids.filter((id) => !isPending(id))
    set({ pending: state.pending.map((p) => (ids.includes(p.id) ? { ...p, queue } : p)) })
    if (real.length) void queueControl(queue ? { action: 'assign', id: queue, job_ids: real } : { action: 'unassign', job_ids: real }).catch(failToast('Could not change the queue'))
    rebuildJobs()
    return
  }
  if (LIVE) {
    meta(ids.filter((id) => !isPending(id)), (m) => ({ ...m, queue }))
    set({
      pending: state.pending.map((p) => (ids.includes(p.id) ? { ...p, queue } : p)),
      queues: state.queues.map((q) => ({ ...q, order: q.id === queue ? [...q.order.filter((x) => !ids.includes(x)), ...ids] : q.order.filter((x) => !ids.includes(x)) })),
    })
    rebuildJobs()
    return
  }
  set({
    jobs: state.jobs.map((j) => (ids.includes(j.id) ? note({ ...j, queue, state: j.state === 'paused' && queue ? 'queued' : j.state }, queue ? `Added to ${queueName(queue)}` : 'Removed from its queue') : j)),
    queues: state.queues.map((q) => ({ ...q, order: q.id === queue ? [...q.order.filter((x) => !ids.includes(x)), ...ids] : q.order.filter((x) => !ids.includes(x)) })),
  })
}

export function setPriority(ids: string[], priority: Job['priority']): void {
  if (LIVE) {
    meta(ids, (m) => ({ ...m, priority }))
    rebuildJobs()
    return
  }
  set({ jobs: state.jobs.map((j) => (ids.includes(j.id) ? { ...j, priority } : j)) })
}

export function renameJob(id: string, name: string): string | null {
  const problem = nameProblem(name)
  if (problem) return problem
  if (LIVE) {
    if (isPending(id)) {
      set({ pending: state.pending.map((p) => (p.id === id ? { ...p, name } : p)) })
      rebuildJobs()
      return null
    }
    if (NATIVE) {
      const f = state.files.find((x) => x.jobId === id && !x.isFolder)
      if (!f) return 'That file is not in the download folders (it was moved or removed).'
      void filesApi.rename(joinPath(f.dir, f.name), name).then(
        () => {
          toast({ kind: 'ok', title: 'Renamed', desc: name, important: false, topic: 'system' })
          nudgeIndexSync()
        },
        (e: unknown) => toast({ kind: 'warn', title: 'Not renamed', desc: e instanceof Error ? e.message : '', important: true, topic: 'system' }),
      )
      return null
    }
    return "Renaming a file needs file write permission, which this client's restricted zeusd credential does not have. Use zeus files organize on your own machine."
  }
  patchJob(id, (j) => note({ ...j, name, category: categoryOf(name, state.settings.categories) }, `Renamed to ${name}`))
  set({ files: state.files.map((f) => (f.jobId === id ? { ...f, name } : f)) })
  return null
}

export function moveJob(id: string, dir: string): string | null {
  if (LIVE) {
    if (isPending(id)) {
      const root = state.settings.saveTo.root
      if (dir !== root && !dir.startsWith(root + '\\')) return `Choose a folder inside ${root}.`
      set({ pending: state.pending.map((p) => (p.id === id ? { ...p, dir } : p)) })
      rebuildJobs()
      return null
    }
    if (NATIVE) {
      const f = state.files.find((x) => x.jobId === id && !x.isFolder)
      if (!f) return 'That file is not in the download folders (it was moved or removed).'
      void filesApi.move(joinPath(f.dir, f.name), dir).then(
        () => {
          toast({ kind: 'ok', title: 'Moved', desc: dir, important: false, topic: 'system' })
          nudgeIndexSync()
        },
        (e: unknown) => toast({ kind: 'warn', title: 'Not moved', desc: e instanceof Error ? e.message : '', important: true, topic: 'system' }),
      )
      return null
    }
    return "Moving a file needs file write permission, which this client's restricted zeusd credential does not have."
  }
  if (!state.settings.saveTo.writeRoots.some((r) => dir === r || dir.startsWith(r + '\\'))) return 'That folder is outside the download folders Zeus may write to. Add it in Settings, Save to.'
  patchJob(id, (j) => note({ ...j, dir }, `Moved to ${dir}`))
  set({ files: state.files.map((f) => (f.jobId === id ? { ...f, dir } : f)) })
  return null
}

export interface NewDownload {
  url: string
  name?: string
  dir?: string
  queue: string | null
  start: boolean
  source?: JobSource
  size?: number | null
  expectedSha256?: string
  project?: string
  description?: string
  sessionOrigin?: string
  /** zeusd's policy for a name that exists (the person's duplicate choice, or the batch default). */
  overwrite?: Overwrite
}

/** Adds downloads, refusing what zeusd refuses. Returns the new job ids, or a reason. */
export function addDownloads(list: NewDownload[]): { ids: string[]; refused: string[] } {
  if (LIVE) return addLive(list)
  const ids: string[] = []
  const refused: string[] = []
  const now = Date.now()
  const jobs: Job[] = []
  for (const d of list) {
    const parsed = intake(d.url)
    const link = parsed.links[0]
    if (!link) {
      refused.push(parsed.refused[0]?.reason ?? `${d.url.slice(0, 60)} is not an http or https address.`)
      continue
    }
    const name = d.name?.trim() || link.name
    const problem = nameProblem(name)
    if (problem) {
      refused.push(`${name}: ${problem}`)
      continue
    }
    const category = categoryOf(name, state.settings.categories)
    const size = d.size !== undefined ? d.size : sizeFor(link.url)
    const job = makeJob({ name, size, host: link.host, state: d.start ? 'probing' : 'queued', source: d.source ?? YOU, queue: d.queue, addedAgo: 0, connections: state.settings.downloads.connectionsPerJob, ...(d.project ? { project: d.project } : {}), ...(d.description ? { description: d.description } : {}) }, now, Math.random)
    job.url = link.display
    job.queryWithheld = link.queryWithheld
    job.category = category
    job.dir = d.dir ?? defaultDir(category)
    job.probeUntil = now + 900
    job.script = { maxRate: (1.5 + Math.random() * 6) * MiB }
    if (d.expectedSha256) job.expectedSha256 = d.expectedSha256
    if (d.sessionOrigin) job.sessionOrigin = d.sessionOrigin
    jobs.push(job)
    ids.push(job.id)
  }
  set({
    jobs: [...jobs, ...state.jobs],
    queues: state.queues.map((q) => ({ ...q, order: [...q.order, ...jobs.filter((j) => j.queue === q.id).map((j) => j.id)] })),
  })
  return { ids, refused }
}

/** Live backend: downloads wait as pending items until sent to zeusd (at once, or when their queue starts). */
function addLive(list: NewDownload[]): { ids: string[]; refused: string[] } {
  const ids: string[] = []
  const refused: string[] = []
  const now = Date.now()
  const items: PendingItem[] = []
  for (const d of list) {
    const parsed = intake(d.url)
    const link = parsed.links[0]
    if (!link) {
      refused.push(parsed.refused[0]?.reason ?? `${d.url.slice(0, 60)} is not an http or https address.`)
      continue
    }
    const hostProblem = hostRefusal(link.host)
    if (hostProblem) {
      refused.push(hostProblem)
      continue
    }
    if (d.sessionOrigin) {
      refused.push(`${link.host} needs a site login, which comes only through the Zeus extension. This client has no browser sessions.`)
      continue
    }
    const name = d.name?.trim() || link.name
    const problem = nameProblem(name)
    if (problem) {
      refused.push(`${name}: ${problem}`)
      continue
    }
    const category = categoryOf(name, state.settings.categories)
    const id = newPendingId()
    items.push({ id, url: link.url, display: link.display, host: link.host, queryWithheld: link.queryWithheld, name, dir: d.dir ?? defaultDir(category), queue: d.start ? null : d.queue, added: now, source: d.source ?? YOU, size: d.size ?? null, ...(d.project ? { project: d.project } : {}), ...(d.description ? { description: d.description } : {}), ...(d.expectedSha256 ? { expectedSha256: d.expectedSha256 } : {}), ...(d.name?.trim() && d.name.trim() !== link.name ? { nameChosen: true } : {}), ...(d.overwrite ? { overwrite: d.overwrite } : {}) })
    ids.push(id)
  }
  set({ pending: [...items, ...state.pending], queues: state.queues.map((q) => ({ ...q, order: [...q.order, ...items.filter((p) => p.queue === q.id).map((p) => p.id)] })) })
  rebuildJobs()
  // In the app's engine every download goes to zeusd at once; a queue holds it there (the label zeus.queue).
  const startNow = items.filter((p) => NATIVE || p.queue === null).map((p) => p.id)
  if (startNow.length) void liveSend(startNow)
  return { ids, refused }
}

export function defaultDir(category: CategoryId): string {
  const s = state.settings.saveTo
  if (NATIVE) return folderFor(category, s, joinPath, SEP() === '\\')
  if (!s.perCategory || category === 'general') return s.root
  const folder = { compressed: 'Compressed', documents: 'Documents', music: 'Music', programs: 'Programs', video: 'Video', pictures: 'Pictures' }[category]
  return joinPath(s.root, folder)
}

/** Remember the folder a download of `category` went to, when the person asked Zeus to remember it. */
export function rememberFolder(category: CategoryId, dir: string): void {
  const s = state.settings.saveTo
  if (!s.rememberLast || !dir || (s.lastDirs ?? {})[category] === dir) return
  if (dir === defaultDirWithout(category)) return
  updateSettings('saveTo', { lastDirs: { ...(s.lastDirs ?? {}), [category]: dir } })
}

function defaultDirWithout(category: CategoryId): string {
  const s = state.settings.saveTo
  return folderFor(category, { ...s, lastDirs: {}, rememberLast: false }, joinPath, SEP() === '\\')
}

/** The downloads and waiting items a new link might duplicate. */
export function existingDownloads(): Existing[] {
  return [
    ...state.jobs.filter((j) => !isPending(j.id)).map((j) => ({ id: j.id, url: j.url, name: j.name, state: j.state })),
    ...state.pending.filter((p) => !p.failed).map((p) => ({ id: p.id, url: p.display, name: p.name, state: 'pending' })),
  ]
}

/** The duplicate of `url` among existing downloads, in the shape a grabber row keeps. */
export function duplicateOf(url: string): GrabberLink['dupe'] | undefined {
  const d = findDuplicate(url, existingDownloads())
  return d ? { id: d.id, state: d.state, name: d.name, where: duplicateWhere(d) } : undefined
}

function hashString(s: string): number {
  let h = 2166136261
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619)
  return h >>> 0
}

/** A plausible size for a sample address, the same every time for the same address. */
export function sizeFor(url: string): number {
  const h = hashString(url)
  const ext = extOf(url.split('?')[0] ?? url)
  const base = ['mp4', 'mkv', 'iso', 'zst'].includes(ext) ? 300 * MiB : ['zip', '7z', 'tar', 'gz', 'exe', 'msi'].includes(ext) ? 40 * MiB : ['mp3', 'flac'].includes(ext) ? 12 * MiB : 2 * MiB
  return Math.round(base * (0.2 + (h % 1000) / 250))
}

export function queueName(id: string | null): string {
  if (!id) return '–'
  return state.queues.find((q) => q.id === id)?.name ?? id
}

// ---------------------------------------------------------------- queues and the limiter

export function startQueue(id: string, why: 'you' | 'schedule' = 'you'): void {
  const q = state.queues.find((x) => x.id === id)
  if (!q) return
  if (state.killSwitch) {
    toast({ kind: 'warn', title: `${q.name} did not start`, desc: 'The global stop is on. A schedule never releases it.', important: true, topic: 'queue' })
    return
  }
  const waiting = state.jobs.filter((j) => j.queue === id && (j.state === 'queued' || j.state === 'paused')).length
  if (NATIVE) {
    void queueControl({ action: 'start', id })
      .then(() => toast({ kind: '', title: `${q.name} started${why === 'schedule' ? ' on schedule' : ''}`, desc: `${waiting} file${waiting === 1 ? '' : 's'} waiting, ${q.maxConcurrent} at a time`, important: why === 'schedule', topic: 'queue', to: '#/queues' }))
      .catch(failToast(`${q.name} did not start`))
    return
  }
  if (LIVE) {
    const paused = state.jobs.filter((j) => j.queue === id && !isPending(j.id) && j.state === 'paused').map((j) => j.id)
    if (paused.length) void liveControl(paused, 'resume')
    set({ queues: state.queues.map((x) => (x.id === id ? { ...x, running: true, lastStarted: Date.now() } : x)) })
    toast({ kind: '', title: `${q.name} started${why === 'schedule' ? ' on schedule' : ''}`, desc: `${waiting} file${waiting === 1 ? '' : 's'} waiting, ${q.maxConcurrent} at a time; each goes to zeusd when its turn comes`, important: why === 'schedule', topic: 'queue', to: '#/queues' })
    return
  }
  set({
    queues: state.queues.map((x) => (x.id === id ? { ...x, running: true, lastStarted: Date.now() } : x)),
    jobs: state.jobs.map((j) => (j.queue === id && j.state === 'paused' ? { ...j, state: 'queued' } : j)),
  })
  toast({ kind: '', title: `${q.name} started${why === 'schedule' ? ' on schedule' : ''}`, desc: q.kind === 'sync' ? 'Checking finished files for newer versions' : `${waiting} file${waiting === 1 ? '' : 's'} waiting, ${q.maxConcurrent} at a time`, important: why === 'schedule', topic: 'queue', to: '#/queues' })
}

export function stopQueue(id: string, why: 'you' | 'schedule' | 'finished' = 'you'): void {
  const q = state.queues.find((x) => x.id === id)
  if (!q) return
  if (NATIVE) {
    void queueControl({ action: 'stop', id })
      .then(() => toast({ kind: '', title: `${q.name} stopped`, desc: 'Its downloads were paused in zeusd; saved bytes stay.', important: false, topic: 'queue' }))
      .catch(failToast(`${q.name} did not stop`))
    return
  }
  if (LIVE) {
    set({ queues: state.queues.map((x) => (x.id === id ? { ...x, running: false } : x)) })
    const active = state.jobs.filter((j) => j.queue === id && !isPending(j.id) && ['queued', 'probing', 'downloading', 'retry_scheduled'].includes(j.state)).map((j) => j.id)
    if (active.length) void liveControl(active, 'pause')
    if (why !== 'finished') toast({ kind: '', title: `${q.name} stopped${why === 'schedule' ? ' on schedule' : ''}`, desc: active.length ? 'Its active downloads were paused in zeusd; saved bytes stay.' : 'Nothing was running.', important: false, topic: 'queue' })
    return
  }
  set({
    queues: state.queues.map((x) => (x.id === id ? { ...x, running: false } : x)),
    jobs: state.jobs.map((j) => (j.queue === id && isActive(j) && j.state !== 'verifying' && j.state !== 'finalizing' ? note(stopSegments({ ...j, state: 'queued' }), `${q.name} stopped; saved bytes kept`) : j)),
  })
  if (why !== 'finished') toast({ kind: '', title: `${q.name} stopped${why === 'schedule' ? ' on schedule' : ''}`, desc: 'Active files went back to waiting with their saved bytes.', important: false, topic: 'queue' })
}

export function updateQueue(id: string, patch: Partial<Queue>): void {
  const next = state.queues.map((q) => (q.id === id ? { ...q, ...patch } : q))
  set({ queues: next })
  if (NATIVE) {
    const q = next.find((x) => x.id === id)
    if (q) void putQueue(q).catch(failToast('Could not save the queue'))
  }
}

export function addQueue(name: string): string {
  const id = NATIVE ? `q${Date.now().toString(36)}`.toLowerCase() : `q${Date.now().toString(36)}`
  const q: Queue = {
    id,
    name: name.trim() || 'New queue',
    kind: 'download',
    builtin: false,
    running: false,
    maxConcurrent: 2,
    retries: 5,
    schedule: { startEnabled: false, startTime: '22:00', repeat: 'days', startDate: '', days: [0, 1, 2, 3, 4, 5, 6], stopEnabled: false, stopTime: '06:00' },
    onFinish: 'notify',
    limitKiBps: null,
    syncEveryHours: 24,
    order: [],
  }
  set({ queues: [...state.queues, q] })
  if (NATIVE) void putQueue(q).catch(failToast('Could not create the queue'))
  return id
}

export function deleteQueue(id: string): void {
  const q = state.queues.find((x) => x.id === id)
  if (!q || q.builtin) return
  if (NATIVE) {
    void deleteQueueRemote(id).catch(failToast('Could not delete the queue'))
    set({ queues: state.queues.filter((x) => x.id !== id), pending: state.pending.map((p) => (p.queue === id ? { ...p, queue: null } : p)) })
    return
  }
  if (LIVE) {
    set({ queues: state.queues.filter((x) => x.id !== id), pending: state.pending.map((p) => (p.queue === id ? { ...p, queue: null } : p)) })
    meta(Object.keys(state.jobMeta).filter((k) => state.jobMeta[k]?.queue === id), (m) => ({ ...m, queue: null }))
    rebuildJobs()
    return
  }
  set({ queues: state.queues.filter((x) => x.id !== id), jobs: state.jobs.map((j) => (j.queue === id ? { ...j, queue: null, state: j.state === 'queued' ? 'paused' : j.state } : j)) })
}

export function moveInQueue(queueId: string, jobId: string, dir: -1 | 1): void {
  if (NATIVE) {
    void queueControl({ action: 'move', job_id: jobId, offset: dir }).catch(failToast('Could not move the download'))
    return
  }
  set({
    queues: state.queues.map((q) => {
      if (q.id !== queueId) return q
      const order = [...q.order]
      const i = order.indexOf(jobId)
      const k = i + dir
      if (i < 0 || k < 0 || k >= order.length) return q
      ;[order[i], order[k]] = [order[k] as string, order[i] as string]
      return { ...q, order }
    }),
  })
}

export function setLimiter(patch: Partial<SpeedLimiter>): void {
  const next = { ...state.limiter, ...patch }
  set({ limiter: next })
  if (NATIVE) void configureLimiter({ enabled: next.enabled, limitKiBps: next.limitKiBps, scheduleEnabled: next.scheduleEnabled, from: next.from, to: next.to, days: next.days }).catch(failToast('Could not change the speed limiter'))
}

/** The global stop: pauses every transfer and revokes every site permission. Releasing restores neither. */
export function globalStop(): void {
  if (LIVE) {
    // zeusd's own kill switch is a human operation at a terminal; this client pauses its downloads instead.
    if (NATIVE) for (const q of state.queues.filter((x) => x.running)) void queueControl({ action: 'stop', id: q.id }).catch(() => undefined)
    set({ killSwitch: true, queues: state.queues.map((q) => ({ ...q, running: false })) })
    const active = state.jobs.filter((j) => !isPending(j.id) && ['queued', 'probing', 'downloading', 'retry_scheduled'].includes(j.state)).map((j) => j.id)
    void liveControl(active, 'pause')
    toast({ kind: 'bad', title: 'Global stop', desc: `${active.length} download${active.length === 1 ? '' : 's'} paused in zeusd; queues stopped. zeusd's own kill switch is in the Zeus CLI (zeus daemon kill-switch).`, important: true, topic: 'system' })
    return
  }
  set({
    killSwitch: true,
    jobs: state.jobs.map((j) => (isActive(j) && j.state !== 'finalizing' && j.state !== 'verifying' ? note(stopSegments({ ...j, state: 'paused' }), 'Paused by the global stop') : j.state === 'queued' ? { ...j, state: 'paused' } : j)),
    queues: state.queues.map((q) => ({ ...q, running: false })),
    settings: { ...state.settings, siteLogins: state.settings.siteLogins.filter((l) => l.scope === 'never') },
  })
  toast({ kind: 'bad', title: 'Global stop', desc: 'Every download paused and every site permission revoked.', important: true, topic: 'system' })
}

export function releaseStop(): void {
  set({ killSwitch: false })
  toast({ kind: '', title: 'Global stop released', desc: 'Nothing resumed and no permission came back. Resume what you want.', important: true, topic: 'system' })
}

// ---------------------------------------------------------------- approvals

interface ApprovalInput {
  kind: ApprovalKind
  website: string
  requester: ApprovalFacts['requester']
  forText: string
  files: { name: string; size: number | null }[]
  destination: string
  jobIds: string[]
  agentSays?: string
  namesRun?: boolean
  expiresIn?: number
  issued?: number
}

function buildApproval(id: string, revision: number, a: ApprovalInput): Approval {
  const issued = a.issued ?? Date.now()
  const scopes = allowedScopes(a.kind, { coversJobs: a.jobIds.length > 0, namesRun: !!a.namesRun })
  const facts: ApprovalFacts = {
    t: 'zeus.approval.facts.v1',
    approval_id: id,
    revision,
    kind: a.kind,
    website: a.website,
    requester: a.requester,
    for: a.forText,
    files: a.files.map((f) => ({ name: f.name, size: f.size === null ? null : Math.round(f.size) })),
    destination: a.destination,
    allowed_scopes: scopes,
    confirmation: confirmationFor(a.kind, scopes.includes('workflow') ? 'workflow' : undefined),
    issued_at: new Date(issued).toISOString(),
    expires_at: new Date(issued + (a.expiresIn ?? DAY)).toISOString(),
  }
  const hash = requestHash(facts)
  return { id, state: 'pending', facts, requestHash: hash, bindCode: bindCode(hash), ...(a.agentSays ? { agentSays: a.agentSays } : {}), jobIds: a.jobIds }
}

function forText(files: { size: number | null }[]): string {
  const known = files.filter((f) => f.size !== null).reduce((n, f) => n + (f.size ?? 0), 0)
  const unknown = files.some((f) => f.size === null)
  return `${files.length} file${files.length === 1 ? '' : 's'}, ${fmtBytes(known)}${unknown ? ' plus files of unknown size' : ''}`
}

export function createApproval(a: Omit<ApprovalInput, 'forText'> & { forText?: string }): Approval {
  const id = ulid('ap', Date.now())
  const ap: Approval = { ...buildApproval(id, 1, { ...a, forText: a.forText ?? forText(a.files) }), ...(LIVE ? { heldBy: 'service' as const } : {}) }
  if (SVC) {
    const { heldBy: _h, ...wire } = ap
    svc.createApproval(wire).catch(() => toast({ kind: 'warn', title: 'Request not stored', desc: 'The request was not kept; it holds for this window only.', important: false, topic: 'system' }))
  }
  set({ approvals: [ap, ...state.approvals], jobs: state.jobs.map((j) => (a.jobIds.includes(j.id) ? note({ ...stopSegments(j), state: 'awaiting_approval', approvalId: id }, `Waiting for your approval (${ap.bindCode})`) : j)) })
  const hosts = [...new Set(state.approvals.filter((x) => x.state === 'pending').map((x) => hostOf(x.facts.website)))]
  const n = state.approvals.filter((x) => x.state === 'pending').length
  // Notification content follows spec 07 section 11: kind, count and host only; never names, addresses or the agent's words.
  toast({ kind: 'warn', title: `Zeus: ${n} request${n === 1 ? ' needs' : 's need'} your decision`, desc: hosts.join(', '), important: true, topic: 'approval', to: `#/approvals/${id}` })
  return ap
}

function hostOf(website: string): string {
  try {
    return new URL(website).hostname
  } catch {
    return website
  }
}

/** A changed request becomes a new revision with a new hash: a decision bound to the old hash is refused. */
export function reviseApproval(id: string, change: { addFile?: { name: string; size: number | null }; jobId?: string }): void {
  const ap = state.approvals.find((x) => x.id === id)
  if (!ap || ap.state !== 'pending') return
  const files = change.addFile ? [...ap.facts.files, change.addFile] : ap.facts.files
  const jobIds = change.jobId ? [...ap.jobIds, change.jobId] : ap.jobIds
  const next = buildApproval(id, ap.facts.revision + 1, {
    kind: ap.facts.kind,
    website: ap.facts.website,
    requester: ap.facts.requester,
    forText: forText(files),
    files,
    destination: ap.facts.destination,
    jobIds,
    ...(ap.agentSays ? { agentSays: ap.agentSays } : {}),
    namesRun: ap.facts.allowed_scopes.includes('workflow'),
    issued: Date.now(),
    expiresIn: Date.parse(ap.facts.expires_at) - Date.now(),
  })
  // The deadline stays fixed across revisions.
  next.facts.expires_at = ap.facts.expires_at
  next.requestHash = requestHash(next.facts)
  next.bindCode = bindCode(next.requestHash)
  set({ approvals: state.approvals.map((x) => (x.id === id ? next : x)) })
}

export type DecideResult = { ok: true } | { ok: false; reason: string }

/** Decides an approval. Reached only from the Approvals page, after the facts, the arming delay and, for a typed-code confirmation, the code. */
export function decide(id: string, decision: 'approve' | 'deny', scope: Scope | undefined, seenHash: string, armedFor: number): DecideResult {
  if (LIVE) return decideLive(id, decision, scope, seenHash, armedFor)
  const ap = state.approvals.find((x) => x.id === id)
  if (!ap) return { ok: false, reason: 'No such request.' }
  if (ap.state !== 'pending') return { ok: false, reason: `This request is already ${ap.state}.` }
  if (ap.requestHash !== seenHash) return { ok: false, reason: `The request changed after you read it (now revision ${ap.facts.revision}). Read the facts again.` }
  if (decision === 'approve' && armedFor < ARMING_MS) return { ok: false, reason: 'The approve button was pressed before it armed.' }
  if (ap.facts.kind === 'cloud_delivery') return { ok: false, reason: 'Cloud delivery is decided only at a verified terminal.' }
  if (decision === 'approve' && ap.facts.allowed_scopes.length && (!scope || !ap.facts.allowed_scopes.includes(scope))) return { ok: false, reason: 'Choose one of the offered scopes.' }
  const now = Date.now()
  const decided: Approval = { ...ap, state: decision === 'approve' && scope !== 'never' ? 'approved' : 'denied', decided: { at: now, decision, ...(scope ? { scope } : {}), via: 'Zeus app' } }
  let jobs = state.jobs
  let settings = state.settings
  let files = state.files
  let projects = state.projects
  const kind = ap.facts.kind
  if (kind === 'session_grant' || kind === 'handoff') {
    if (decision === 'approve' && scope !== 'never') {
      jobs = jobs.map((j) => (ap.jobIds.includes(j.id) ? note({ ...j, state: 'probing', probeUntil: now + 900 }, `You approved ${hostOf(ap.facts.website)} (${scope}); the extension supplies the session in memory only`) : j))
      settings = { ...settings, siteLogins: [{ origin: ap.facts.website, label: ap.facts.requester.label, scope: scope ?? 'once', ...(scope === 'site' ? { expires: now + DAY } : {}), via: 'extension' }, ...settings.siteLogins.filter((l) => l.origin !== ap.facts.website)] }
    } else {
      const never = scope === 'never'
      jobs = jobs.map((j) => (ap.jobIds.includes(j.id) ? note({ ...stopSegments(j), state: 'failed', error: { code: never ? 'policy.never' : 'policy.denied', message: never ? 'You blocked this site.' : 'You denied the site login.', next: never ? 'Remove the block in Settings, Site logins to ask again.' : 'Ask again only if you change your mind.' } }, never ? 'You blocked this site' : 'You denied the request') : j))
      if (never) settings = { ...settings, siteLogins: [{ origin: ap.facts.website, label: 'Blocked by you', scope: 'never', via: 'extension' }, ...settings.siteLogins.filter((l) => l.origin !== ap.facts.website)] }
    }
  } else if (kind === 'destructive_op' && decision === 'approve') {
    const moves = pendingMoves.get(id) ?? []
    files = files.map((f) => {
      const m = moves.find((x) => x.fileId === f.id)
      return m ? { ...f, dir: m.toDir } : f
    })
    jobs = jobs.map((j) => {
      const m = moves.find((x) => x.jobId === j.id)
      return m ? note({ ...j, dir: m.toDir }, `Moved to ${m.toDir} (approved organize)`) : j
    })
    toast({ kind: 'ok', title: 'Files organized', desc: `${moves.length} moved; undo is available with zeus files undo`, important: false, topic: 'system' })
  } else if (kind === 'workflow_plan') {
    const p = projects.find((x) => x.approvalId === id)
    if (p) {
      if (decision === 'approve') {
        projects = projects.map((x) => (x.id === p.id ? { ...x, state: 'running' } : x))
        set({ approvals: state.approvals.map((x) => (x.id === id ? decided : x)), projects })
        const preview = previewSite(p.request)
        addDownloads(preview.files.map((f) => ({ url: f.url, name: f.name, queue: p.queue, start: p.queue === null, source: { kind: 'workflow', label: `Site grabber: ${p.name}` }, size: f.size, project: p.name })))
        return { ok: true }
      }
      projects = projects.map((x) => (x.id === p.id ? { ...x, state: 'denied' } : x))
    }
  }
  set({ approvals: state.approvals.map((x) => (x.id === id ? decided : x)), jobs, settings, files, projects })
  return { ok: true }
}

/**
 * Live backend. A request zeusd holds is never decided here: zeusd accepts decisions only from a person on its own
 * trust surfaces (the interactive CLI, the extension popup). Requests the pre-alpha service holds (site-grabber plans
 * and file moves proposed in this client) are decided here with the same rules, and the service checks them again.
 */
function decideLive(id: string, decision: 'approve' | 'deny', scope: Scope | undefined, seenHash: string, armedFor: number): DecideResult {
  const ap = state.approvals.find((x) => x.id === id)
  if (!ap) return { ok: false, reason: 'No such request.' }
  if (ap.heldBy === 'zeusd') return { ok: false, reason: 'zeusd holds this request. Decide it in the Zeus CLI (zeus approvals) or the extension popup; no web page can decide a zeusd approval.' }
  if (ap.state !== 'pending') return { ok: false, reason: `This request is already ${ap.state}.` }
  if (ap.requestHash !== seenHash) return { ok: false, reason: `The request changed after you read it (now revision ${ap.facts.revision}). Read the facts again.` }
  if (decision === 'approve' && armedFor < ARMING_MS) return { ok: false, reason: 'The approve button was pressed before it armed.' }
  if (decision === 'approve' && ap.facts.allowed_scopes.length && (!scope || !ap.facts.allowed_scopes.includes(scope))) return { ok: false, reason: 'Choose one of the offered scopes.' }
  const now = Date.now()
  const decided: Approval = { ...ap, state: decision === 'approve' && scope !== 'never' ? 'approved' : 'denied', decided: { at: now, decision, ...(scope ? { scope } : {}), via: NATIVE ? 'Zeus app (this window)' : 'Pre-alpha service (this page)' } }
  const apply = () => {
    set({ approvals: state.approvals.map((x) => (x.id === id ? decided : x)) })
    const kind = ap.facts.kind
    if (kind === 'workflow_plan') {
      const p = state.projects.find((x) => x.approvalId === id)
      if (!p) return
      if (decision === 'approve' && scope !== 'never') {
        set({ projects: state.projects.map((x) => (x.id === p.id ? { ...x, state: 'running' } : x)) })
        const files = p.found ?? []
        const res = addDownloads(files.map((f) => ({ url: f.url, name: f.name, queue: p.queue, start: p.queue === null, source: { kind: 'workflow', label: `Site grabber: ${p.name}` }, size: f.size, project: p.name })))
        if (res.refused.length) toast({ kind: 'warn', title: `${res.refused.length} file${res.refused.length === 1 ? '' : 's'} refused`, desc: res.refused[0] ?? '', important: false, topic: 'system' })
      } else set({ projects: state.projects.map((x) => (x.id === p.id ? { ...x, state: 'denied' } : x)) })
    } else if (kind === 'destructive_op' && decision === 'approve') {
      toast({ kind: 'warn', title: 'Approved, nothing moved', desc: "Your decision is recorded. Moving files needs zeusd's files.write permission, which this client's restricted credential does not have; zeus files organize does it on your own machine.", important: true, topic: 'system' })
    }
  }
  if (!SVC) {
    apply()
    return { ok: true }
  }
  void svc
    .decide(id, { decision, ...(scope ? { scope } : {}), seen_hash: seenHash, armed_ms: Math.round(armedFor) })
    .then(apply)
    .catch((e: unknown) => toast({ kind: 'bad', title: 'Decision refused', desc: e instanceof Error ? e.message : 'The pre-alpha service refused the decision.', important: true, topic: 'approval' }))
  return { ok: true }
}

const pendingMoves = new Map<string, { fileId: number; jobId?: string; toDir: string }[]>()

/** An organize request from the agent's preview: the moves wait for a `destructive_op` approval. */
export function requestOrganize(moves: { fileId: number; jobId?: string; name: string; fromDir: string; toDir: string }[], requester: ApprovalFacts['requester'], agentSays?: string): Approval {
  const ap = createApproval({
    kind: 'destructive_op',
    website: 'local files',
    requester,
    forText: `move ${moves.length} file${moves.length === 1 ? '' : 's'} inside ${rootDir()}`,
    files: moves.map((m) => ({ name: `${m.name} → ${m.toDir.replace(rootDir(), '…')}`, size: null })),
    destination: rootDir(),
    jobIds: [],
    ...(agentSays ? { agentSays } : {}),
  })
  pendingMoves.set(ap.id, moves)
  return ap
}

function sampleApprovals(now: number, s: State): Approval[] {
  const handoffJob = makeJob({ name: 'journal-article-4471.pdf', size: 3.4 * MiB, host: 'reader.example.org', path: 'articles', state: 'awaiting_approval', source: EDGE, addedAgo: 4 * MIN, query: true }, now, Math.random)
  const ap1 = buildApproval(ulid('ap', now - 4 * MIN), 1, {
    kind: 'handoff',
    website: 'https://reader.example.org',
    requester: { kind: 'human', label: 'Edge, browser handoff' },
    forText: '1 file, 3.40 MiB',
    files: [{ name: handoffJob.name, size: handoffJob.size }],
    destination: `${ROOT}\\Documents`,
    jobIds: [handoffJob.id],
    issued: now - 4 * MIN,
  })
  handoffJob.approvalId = ap1.id
  handoffJob.sessionOrigin = 'https://reader.example.org'
  s.jobs = [handoffJob, ...s.jobs]
  const past = (kind: ApprovalKind, website: string, label: string, files: { name: string; size: number | null }[], ago: number, decision: 'approve' | 'deny', scope: Scope | undefined, via: string, st?: 'expired'): Approval => {
    const a = buildApproval(ulid('ap', now - ago), 1, { kind, website, requester: { kind: label.includes('MCP') ? 'agent' : 'human', label }, forText: forText(files), files, destination: `${ROOT}\\Documents`, jobIds: [], issued: now - ago })
    if (st) return { ...a, state: 'expired' }
    return { ...a, state: decision === 'approve' && scope !== 'never' ? 'approved' : 'denied', decided: { at: now - ago + 40_000, decision, ...(scope ? { scope } : {}), via } }
  }
  return [
    ap1,
    past('session_grant', 'https://library.example.edu', 'Claude Code (MCP)', [{ name: 'chapter-03.pdf', size: 2.1 * MiB }, { name: 'chapter-04.pdf', size: 2.6 * MiB }], 5 * HOUR, 'approve', 'site', 'Zeus extension popup'),
    past('session_grant', 'https://files.example.net', 'Cursor (MCP)', [{ name: 'q3-numbers.xlsx', size: 0.4 * MiB }], 26 * HOUR, 'approve', 'once', 'terminal (zeus approvals)'),
    past('session_grant', 'https://ads.example.com', 'Claude Code (MCP)', [{ name: 'offer.zip', size: null }], 3 * DAY, 'deny', 'never', 'terminal (zeus approvals)'),
    past('destructive_op', 'local files', 'Claude Code (MCP)', [{ name: '12 files → Documents\\Papers', size: null }], 4 * DAY, 'approve', undefined, 'terminal (zeus approvals)'),
    past('workflow_plan', 'https://docs.example.org', 'You, in Zeus', [{ name: '4 files from docs.example.org', size: 54 * MiB }], 8 * DAY, 'approve', undefined, 'terminal (zeus approvals)'),
    past('session_grant', 'https://portal.example.edu', 'Cursor (MCP)', [{ name: 'syllabus.pdf', size: 0.3 * MiB }], 6 * DAY, 'approve', 'once', 'Zeus extension popup', 'expired'),
  ]
}

// ---------------------------------------------------------------- link grabber

export function grabberAdd(text: string): ReturnType<typeof intake> {
  const r = intake(text)
  const existing = new Set(state.grabber.map((g) => g.url))
  const fresh: GrabberLink[] = r.links
    .filter((l) => !existing.has(l.url))
    .map((l) => {
      const category = categoryOf(l.name, state.settings.categories)
      const dupe = duplicateOf(l.url)
      return { id: `gl${hashString(l.url).toString(36)}`, url: l.url, display: l.display, host: l.host, queryWithheld: l.queryWithheld, name: l.name, check: 'unchecked' as const, size: null, type: extOf(l.name) || 'page', category, dir: defaultDir(category), selected: !dupe, pkg: l.host, ...(dupe ? { dupe, dupeChoice: 'skip' as const } : {}) }
    })
  const dup = r.links.length - fresh.length
  set({ grabber: [...state.grabber, ...fresh].slice(0, 50) })
  return { ...r, duplicates: r.duplicates + dup }
}

/** Online check on sample answers: the same address always gives the same answer. Nothing is fetched. */
export function grabberCheck(ids: string[]): void {
  set({ grabber: state.grabber.map((g) => (ids.includes(g.id) ? { ...g, check: 'checking' } : g)) })
  if (LIVE) {
    const links = state.grabber.filter((g) => ids.includes(g.id))
    const done = (results: Map<string, { check: GrabberLink['check']; size: number | null; name?: string | null; note?: string; status?: number; resumable?: GrabberLink['resumable'] }>) =>
      set({ grabber: state.grabber.map((g) => {
        const r = results.get(g.id)
        if (!r || g.check !== 'checking') return g
        const named = r.name && r.name !== g.name ? r.name : null
        const category = named ? categoryOf(named, state.settings.categories) : g.category
        // A name the server gives can change the category, and with it the folder, unless the folder was typed.
        const dir = named && !g.dirEdited ? defaultDir(category) : g.dir
        const bad = r.check === 'offline' || (NATIVE && r.check === 'needs_login')
        return { ...g, check: r.check, size: r.size, ...(named ? { name: named, type: extOf(named) || g.type, category, dir } : {}), ...(r.note !== undefined ? { note: r.note } : {}), ...(r.status !== undefined ? { status: r.status } : {}), ...(r.resumable !== undefined ? { resumable: r.resumable } : {}), selected: bad ? false : g.selected }
      }) })
    if (SVC) {
      // Rows answer in groups of six, so a long list shows progress instead of waiting for the last link.
      void (async () => {
        for (let at = 0; at < links.length; at += 6) {
          const group = links.slice(at, at + 6)
          try {
            const r = await svc.check(group.map((l) => l.url))
            done(new Map(group.map((l, i) => {
              const a = r.items[i]
              return [l.id, a ? { check: a.check, size: a.size ?? null, name: a.name ?? null, note: checkNote(a), status: a.status, resumable: a.resumable ?? null } : { check: 'unchecked' as const, size: null }]
            })))
          } catch {
            done(new Map(group.map((l) => [l.id, { check: 'unchecked' as const, size: null, note: 'The check did not answer. Try again.' }])))
          }
        }
      })()
    } else {
      void Promise.all(links.map((l) => api.preview([l.url], '').then((r) => ({ check: (r.ready ? 'online' : 'offline') as GrabberLink['check'], size: null }), () => ({ check: 'unchecked' as const, size: null })))).then((rs) => done(new Map(links.map((l, i) => [l.id, rs[i] ?? { check: 'unchecked', size: null }]))))
    }
    return
  }
  setTimeout(() => {
    set({
      grabber: state.grabber.map((g) => {
        if (!ids.includes(g.id) || g.check !== 'checking') return g
        const h = hashString(g.url)
        const offline = /missing|removed|old-|gone/.test(g.url) || h % 13 === 0
        const login = /portal\.|library\.|members\./.test(g.host)
        return { ...g, check: offline ? 'offline' : login ? 'needs_login' : 'online', size: offline ? null : sizeFor(g.url), selected: offline ? false : g.selected }
      }),
    })
  }, 700 + Math.random() * 600)
}

export function grabberUpdate(id: string, patch: Partial<GrabberLink>): void {
  set({ grabber: state.grabber.map((g) => (g.id === id ? { ...g, ...patch } : g)) })
}

export function grabberUpdateMany(ids: string[], patch: Partial<GrabberLink>): void {
  set({ grabber: state.grabber.map((g) => (ids.includes(g.id) ? { ...g, ...patch } : g)) })
}

export function grabberRemove(ids: string[]): void {
  set({ grabber: state.grabber.filter((g) => !ids.includes(g.id)) })
}

/** Step one of the real review flow: resolve destinations without creating jobs. The review is kept for ten minutes. */
export function grabberPreview(ids: string[], opts: { overwrite?: Overwrite } = {}): Review | string {
  // Duplicates are looked up again now: a download may have finished since the link was added.
  const fresh = state.grabber.map((g) => (ids.includes(g.id) ? { ...g, dupe: duplicateOf(g.url) } : g))
  const chosen = fresh.filter((g) => ids.includes(g.id) && g.check !== 'offline')
  const skipped = chosen.filter((g) => g.dupe && (g.dupeChoice ?? 'skip') === 'skip').length
  const links = chosen.filter((g) => !(g.dupe && (g.dupeChoice ?? 'skip') === 'skip'))
  if (!links.length) return skipped ? 'Every selected link is already downloaded and set to be skipped. Choose Download again or Replace for the ones you want.' : 'Select at least one link that is not offline.'
  for (const l of links) {
    const p = nameProblem(l.name)
    if (p) return `${l.name}: ${p}`
  }
  const taken = new Set(state.files.map((f) => `${f.dir}\\${f.name}`.toLowerCase()))
  const destinations = links.map((l) => {
    let name = l.name
    let collides = false
    let n = 1
    while (taken.has(`${l.dir}\\${name}`.toLowerCase())) {
      collides = true
      const dot = l.name.lastIndexOf('.')
      name = dot > 0 ? `${l.name.slice(0, dot)} (${n})${l.name.slice(dot)}` : `${l.name} (${n})`
      n++
    }
    taken.add(`${l.dir}\\${name}`.toLowerCase())
    return { name, dir: l.dir, collides }
  })
  if (LIVE) {
    for (const l of links) {
      const problem = hostRefusal(l.host)
      if (problem) return `${l.host}: ${problem}`
    }
  }
  const review: Review = { id: ulid('rv', Date.now()), created: Date.now(), links, destinations, overwrite: opts.overwrite ?? state.settings.downloads.duplicates, skippedDuplicates: skipped, ...(LIVE ? { remote: { state: 'checking' as const, reviews: [] } } : {}) }
  set({ reviews: [review, ...state.reviews.slice(0, 4)] })
  if (LIVE) void remotePreview(review)
  return review
}

function fileNameFromUrl(url: string): string {
  try {
    const u = new URL(url)
    const seg = u.pathname.split('/').filter(Boolean).pop() ?? u.hostname
    return decodeURIComponent(seg)
  } catch {
    return url
  }
}

/** zeusd resolves the batch without creating jobs; the bridge keeps the exact inputs for ten minutes. */
async function remotePreview(review: Review): Promise<void> {
  if (NATIVE) {
    const finish = (remote: NonNullable<Review['remote']>) => set({ reviews: state.reviews.map((r) => (r.id === review.id ? { ...r, remote } : r)) })
    try {
      const batch = review.overwrite ?? state.settings.downloads.duplicates
      const r = await api.previewItems(review.links.map((l) => ({ url: l.url, dir: l.dir, ...(l.check === 'online' || l.name !== fileNameFromUrl(l.url) ? { filename: l.name } : {}), ...(l.expectedSha256 ? { expected_sha256: l.expectedSha256 } : {}), overwrite: nativeOverwrite(overwriteFor(l.dupeChoice, batch)) })))
      const plan = r.items as PlanRow[]
      if (r.refused.length) return finish({ state: 'refused', reason: `${review.links[r.refused[0]?.index ?? 0]?.host ?? ''}: ${r.refused[0]?.reason ?? ''}`, reviews: [], plan })
      if (!r.ready || !r.review_id) return finish({ state: 'refused', reason: r.items.find((i) => i.error_message)?.error_message ?? 'zeusd could not plan this batch. Check the addresses.', reviews: [], plan })
      finish({ state: 'ready', reviews: [{ reviewId: r.review_id, linkIds: review.links.map((l) => l.id) }], plan })
    } catch (e) {
      finish({ state: 'refused', reason: e instanceof Error ? e.message : 'The review failed.', reviews: [] })
    }
    return
  }
  const groups = new Map<string, GrabberLink[]>()
  for (const l of review.links) groups.set(l.dir, [...(groups.get(l.dir) ?? []), l])
  const reviews: { reviewId: string; linkIds: string[] }[] = []
  const finish = (remote: NonNullable<Review['remote']>) => set({ reviews: state.reviews.map((r) => (r.id === review.id ? { ...r, remote } : r)) })
  try {
    for (const [dir, links] of groups) {
      const r = await api.preview(links.map((l) => l.url), subdirOf(dir))
      if (r.refused.length) return finish({ state: 'refused', reason: `${links[r.refused[0]?.index ?? 0]?.host ?? ''}: ${r.refused[0]?.reason ?? ''}`, reviews })
      if (!r.ready || !r.review_id) return finish({ state: 'refused', reason: 'zeusd could not plan this batch. Check the addresses.', reviews })
      reviews.push({ reviewId: r.review_id, linkIds: links.map((l) => l.id) })
    }
    finish({ state: 'ready', reviews })
  } catch (e) {
    finish({ state: 'refused', reason: e instanceof Error ? e.message : 'The review failed.', reviews })
  }
}

/** Step two: start exactly the reviewed batch. A review older than ten minutes, or one already used, is refused. */
/** zeusd's "replace" needs a terminal-approved destructive operation; the app replaces by deleting first, then renaming. */
function nativeOverwrite(o: 'rename' | 'fail' | 'replace'): 'rename' | 'fail' | 'replace' {
  return NATIVE && o === 'replace' ? 'rename' : o
}

export function grabberStart(reviewId: string, queue: string | null): { ok: true; ids: string[] } | { ok: false; reason: string } {
  const r = state.reviews.find((x) => x.id === reviewId)
  if (!r) return { ok: false, reason: 'This review is no longer available. Preview the downloads again.' }
  if (Date.now() - r.created > 10 * MIN) return { ok: false, reason: 'This review expired after ten minutes. Preview the downloads again.' }
  if (LIVE && (queue === null || NATIVE)) {
    if (!r.remote || r.remote.state === 'checking') return { ok: false, reason: 'zeusd is still checking this batch. Start it in a moment.' }
    if (r.remote.state === 'refused') return { ok: false, reason: r.remote.reason ?? 'zeusd refused this batch.' }
    const remote = r.remote
    set({ reviews: state.reviews.filter((x) => x.id !== reviewId), grabber: state.grabber.filter((g) => !r.links.some((l) => l.id === g.id)) })
    void (async () => {
      let started = 0
      // zeusd's own "replace" needs a terminal-approved destructive operation, so a person's "Replace the old file" is
      // done by this app at the person's click: the old file goes to the recycle bin, then the name is free.
      const replacing = NATIVE ? r.links.filter((l) => l.dupe && l.dupeChoice === 'replace') : []
      if (replacing.length) {
        // The file to move away is the one the new download would land on: a completed download of this address that
        // sits at the destination. (A second copy named "(1)" is another file and stays.) Failing that, the download the
        // duplicate check pointed at.
        const slashes = (x: string) => x.replace(/[\\/]+/g, '/')
        const same = (a: string, b: string) => (SEP() === '\\' ? slashes(a).toLowerCase() === slashes(b).toLowerCase() : slashes(a) === slashes(b))
        const paths = [
          ...new Set(
            replacing.flatMap((l) => {
              const target = joinPath(l.dir, l.name)
              const done = state.jobs.filter((j) => j.state === 'completed' && same(joinPath(j.dir, j.name), target))
              if (done.length) return [target]
              const fallback = state.jobs.find((j) => j.id === l.dupe?.id && j.state === 'completed')
              return fallback ? [joinPath(fallback.dir, fallback.name)] : []
            }),
          ),
        ]
        if (paths.length) {
          try {
            const gone = await filesApi.remove(paths)
            if (gone.failed.length) toast({ kind: 'warn', title: 'Old file not removed', desc: `${gone.failed.length} old file${gone.failed.length === 1 ? '' : 's'} could not be moved to the recycle bin; the new copy gets a new name.`, important: true, topic: 'system' })
          } catch (e) {
            toast({ kind: 'warn', title: 'Old file not removed', desc: e instanceof Error ? e.message : 'The old file stays and the new copy gets a new name.', important: true, topic: 'system' })
          }
        }
      }
      for (const rv of remote.reviews) {
        try {
          const res = await api.enqueue(rv.reviewId, retryKey(), NATIVE ? queue : null)
          const next = { ...state.jobMeta }
          res.jobs.forEach((j) => {
            if (j.job_id) {
              next[j.job_id] = { queue, source: YOU }
              started++
            }
          })
          set({ jobMeta: next })
          for (const l of r.links) rememberFolder(l.category, l.dir)
          const refused = res.jobs.filter((j) => j.error_code)
          if (refused.length) toast({ kind: 'warn', title: `${refused.length} download${refused.length === 1 ? '' : 's'} refused`, desc: refused[0]?.error_message ?? refused[0]?.error_code ?? '', important: true, topic: 'failure' })
        } catch (e) {
          toast({ kind: 'bad', title: 'Not started', desc: e instanceof Error ? e.message : 'zeusd did not start this batch.', important: true, topic: 'failure' })
        }
      }
      if (started) toast({ kind: 'ok', title: `${started} download${started === 1 ? '' : 's'} ${queue ? 'added to the queue' : 'started'}`, desc: `From your reviewed batch${r.skippedDuplicates ? `; ${r.skippedDuplicates} duplicate${r.skippedDuplicates === 1 ? '' : 's'} skipped` : ''}.`, important: false, topic: 'queue', to: '#/downloads' })
      void refreshFiles(false)
    })()
    return { ok: true, ids: [] }
  }
  const res = addDownloads(
    r.links.map((l, i) => ({
      url: l.url,
      name: r.destinations[i]?.name ?? l.name,
      dir: l.dir,
      queue,
      start: queue === null,
      size: l.size,
      ...(l.expectedSha256 ? { expectedSha256: l.expectedSha256 } : {}),
      overwrite: nativeOverwrite(overwriteFor(l.dupeChoice, r.overwrite ?? state.settings.downloads.duplicates)),
      ...(l.check === 'needs_login' ? { sessionOrigin: new URL(l.url).origin } : {}),
    })),
  )
  set({ reviews: state.reviews.filter((x) => x.id !== reviewId), grabber: state.grabber.filter((g) => !r.links.some((l) => l.id === g.id)) })
  return { ok: true, ids: res.ids }
}

// ---------------------------------------------------------------- site grabber

export function siteRequestRun(req: SiteRequest, name: string, queue: string | null, given?: SitePreview): SiteProject | string {
  if (LIVE && !given) return 'Preview the walk first.'
  const preview = given ?? previewSite(req)
  if (!preview.ok) return preview.error ?? 'The preview failed.'
  if (!preview.files.length) return 'Nothing would be fetched with these rules.'
  const id = ulid('proj', Date.now())
  const ap = createApproval({
    kind: 'workflow_plan',
    website: new URL(req.startUrl).origin,
    requester: { kind: 'human', label: 'You, in Zeus' },
    forText: `${preview.files.length} files, ${fmtBytes(preview.totalBytes)}, ${preview.pages.length} pages, depth ${req.depth}`,
    files: preview.files.slice(0, 20).map((f) => ({ name: f.name, size: f.size })),
    destination: rootDir(),
    jobIds: [],
  })
  const p: SiteProject = { id, name: name.trim() || preview.host, startUrl: req.startUrl, request: req, ...(LIVE ? { found: preview.files.map((f) => ({ url: f.url, name: f.name, size: f.size })) } : {}), created: Date.now(), state: 'awaiting_approval', files: preview.files.length, bytes: preview.totalBytes, approvalId: ap.id, queue }
  set({ projects: [p, ...state.projects] })
  return p
}

// ---------------------------------------------------------------- files and the index

export function rescan(path?: string): void {
  if (state.index.state === 'scanning') return
  if (NATIVE) {
    set({ index: { ...state.index, state: 'scanning', progress: 0, scanning: path ?? 'your folders' } })
    void filesApi.rescan(path).then(() => nudgeIndexSync(), () => undefined)
    return
  }
  if (LIVE) {
    set({ index: { ...state.index, state: 'scanning', progress: 0, scanning: path ?? 'your download folder' } })
    void refreshFiles(true).then(() => {
      if (state.index.state === 'scanning') set({ index: { ...state.index, state: 'current', progress: 1, lastScan: Date.now() } })
    })
    return
  }
  set({ index: { ...state.index, state: 'scanning', progress: 0, ...(path ? { scanning: path } : { scanning: 'all folders' }) } })
}

export function addBookmark(name: string, query: string, filter: string): void {
  set({ bookmarks: [...state.bookmarks, { id: `b${Date.now().toString(36)}`, name: name.trim() || query, query, filter }] })
}

export function removeBookmark(id: string): void {
  set({ bookmarks: state.bookmarks.filter((b) => b.id !== id) })
}

export function pushHistory(q: string): void {
  const t = q.trim()
  if (!t) return
  set({ history: [t, ...state.history.filter((h) => h !== t)].slice(0, state.settings.index.maxHistory) })
}

export function clearHistory(): void {
  set({ history: [] })
}

// ---------------------------------------------------------------- settings, skills, connectors

export function updateSettings<K extends keyof Settings>(key: K, patch: Partial<Settings[K]> | Settings[K]): void {
  const cur = state.settings[key]
  const next = Array.isArray(cur) ? (patch as Settings[K]) : ({ ...(cur as object), ...(patch as object) } as Settings[K])
  if (key === 'general') setTimeFormat((next as Settings['general']).timeFormat)
  set({ settings: { ...state.settings, [key]: next } })
}

export function updateSkill(id: string, patch: Partial<Skill>): void {
  set({ skills: state.skills.map((s) => (s.id === id ? { ...s, ...patch } : s)) })
}

export function addSkill(skill: Skill): void {
  set({ skills: [...state.skills, skill] })
}

export function deleteSkill(id: string): void {
  set({ skills: state.skills.filter((s) => s.id !== id || s.builtin) })
}

export function toggleConnector(id: string, on: boolean): void {
  if (!CONNECTORS.some((c) => c.id === id)) return
  set({ connectorsAdded: on ? [...new Set([...state.connectorsAdded, id])] : state.connectorsAdded.filter((x) => x !== id) })
}

export function setThreads(f: (t: Thread[]) => Thread[]): void {
  set({ threads: f(state.threads) })
}

/** Starts again from the sample state. */
export function resetAll(): void {
  if (LIVE) {
    void clearStored().finally(() => window.location.reload())
    return
  }
  state = init()
  scripted.clear()
  emit()
}

// ---------------------------------------------------------------- the clock

const scripted = new Set<string>()

function once(key: string, at: number, f: () => void): void {
  if (scripted.has(key) || state.now - state.t0 < at) return
  scripted.add(key)
  f()
}

function scriptedEvents(): void {
  // An agent asks for a file behind a login: the job waits for a person.
  once('session-request', 6_000, () => {
    const res = addDownloads([{ url: 'https://portal.example.edu/proceedings/conference-proceedings-2026.epub', queue: null, start: false, source: CLAUDE, size: 18.4 * MiB, sessionOrigin: 'https://portal.example.edu' }])
    const id = res.ids[0]
    if (!id) return
    createApproval({
      kind: 'session_grant',
      website: 'https://portal.example.edu',
      requester: { kind: 'agent', label: 'Claude Code (MCP)' },
      files: [{ name: 'conference-proceedings-2026.epub', size: 18.4 * MiB }],
      destination: `${ROOT}\\Documents`,
      jobIds: [id],
      agentSays: 'The proceedings are only available to signed-in members. Please let Zeus use your portal login for this one file.',
    })
  })
  // A second file from the same requester and site joins the pending request: a new revision with a new hash.
  once('session-revision', 50_000, () => {
    const ap = state.approvals.find((a) => a.state === 'pending' && a.facts.kind === 'session_grant' && a.facts.website === 'https://portal.example.edu')
    if (!ap) return
    const res = addDownloads([{ url: 'https://portal.example.edu/proceedings/conference-supplement.pdf', queue: null, start: false, source: CLAUDE, size: 4.1 * MiB, sessionOrigin: 'https://portal.example.edu' }])
    const id = res.ids[0]
    if (!id) return
    set({ jobs: state.jobs.map((j) => (j.id === id ? note({ ...j, state: 'awaiting_approval', approvalId: ap.id }, 'Joined a pending request for the same site') : j)) })
    reviseApproval(ap.id, { addFile: { name: 'conference-supplement.pdf', size: 4.1 * MiB }, jobId: id })
    toast({ kind: 'warn', title: 'A pending request changed', desc: 'portal.example.edu, revision 2', important: false, topic: 'approval', to: `#/approvals/${ap.id}` })
  })
}

function verifyRate(): number {
  return 420 * MiB
}

/** Live backend: schedules and queues run here; transfers run in zeusd. */
function liveTick(now: number, prev: number): void {
  if (NATIVE) {
    if (state.approvals.some((a) => a.state === 'pending' && a.heldBy !== 'zeusd' && Date.parse(a.facts.expires_at) <= now)) {
      set({ approvals: state.approvals.map((a) => (a.state === 'pending' && a.heldBy !== 'zeusd' && Date.parse(a.facts.expires_at) <= now ? { ...a, state: 'expired' } : a)) })
    }
    state = { ...state, now }
    emit()
    return
  }
  for (const q of state.queues) {
    if (!q.running && !state.killSwitch && startDue(q.schedule, prev, now)) startQueue(q.id, 'schedule')
    else if (q.running && stopDue(q.schedule, q.lastStarted, prev, now)) stopQueue(q.id, 'schedule')
  }
  // A running queue sends its waiting downloads to zeusd in order, within its own and the global limit.
  if (!state.killSwitch && state.live?.loaded) {
    const active = (j: Job) => !isPending(j.id) && ['queued', 'probing', 'downloading', 'verifying', 'finalizing', 'retry_scheduled'].includes(j.state)
    let total = state.jobs.filter(active).length + state.pending.filter((p) => p.sending).length
    for (const q of state.queues) {
      if (!q.running) continue
      let inQueue = state.jobs.filter((j) => j.queue === q.id && active(j)).length + state.pending.filter((p) => p.queue === q.id && p.sending).length
      const waiting = state.pending.filter((p) => p.queue === q.id && !p.sending && !p.failed)
      const order = [...q.order]
      waiting.sort((a, b) => (order.indexOf(a.id) + 1 || 1e9) - (order.indexOf(b.id) + 1 || 1e9))
      const batch: string[] = []
      for (const p of waiting) {
        if (inQueue >= q.maxConcurrent || total >= state.settings.downloads.concurrentJobs) break
        batch.push(p.id)
        inQueue++
        total++
      }
      if (batch.length) void liveSend(batch)
      const left = state.pending.some((p) => p.queue === q.id && !p.failed) || state.jobs.some((j) => j.queue === q.id && active(j))
      if (!left && q.kind !== 'sync' && q.lastStarted && now - q.lastStarted > 3000) {
        set({ queues: state.queues.map((x) => (x.id === q.id ? { ...x, running: false } : x)) })
        const action = q.onFinish === 'notify' ? 'Notified you' : q.onFinish === 'stop_limiter' ? 'Turned the speed limiter off' : q.onFinish === 'close_app' ? 'Would close the window (not in this release)' : q.onFinish === 'sleep' ? 'Would put the computer to sleep (not in this release)' : 'Nothing else to do'
        if (q.onFinish === 'stop_limiter') set({ limiter: { ...state.limiter, enabled: false } })
        toast({ kind: 'ok', title: `${q.name} finished`, desc: action, important: true, topic: 'queue' })
      }
    }
  }
  if (state.approvals.some((a) => a.state === 'pending' && a.heldBy !== 'zeusd' && Date.parse(a.facts.expires_at) <= now)) {
    set({ approvals: state.approvals.map((a) => (a.state === 'pending' && a.heldBy !== 'zeusd' && Date.parse(a.facts.expires_at) <= now ? { ...a, state: 'expired' } : a)) })
  }
  state = { ...state, now }
  emit()
}

function tick(): void {
  const now = Date.now()
  const prev = state.now
  if (LIVE) {
    liveTick(now, prev)
    return
  }
  const dt = Math.min(2, (now - prev) / 1000)
  state = { ...state, now }
  scriptedEvents()

  // Queue schedules.
  for (const q of state.queues) {
    if (!q.running && !state.killSwitch && startDue(q.schedule, prev, now)) startQueue(q.id, 'schedule')
    else if (q.running && stopDue(q.schedule, q.lastStarted, prev, now)) stopQueue(q.id, 'schedule')
  }

  const s = state.settings.downloads
  let jobs = state.jobs.slice()
  const activeCount = () => jobs.filter((j) => isActive(j)).length
  const finished: Job[] = []
  const needApproval: Job[] = []

  // The scheduler: queued jobs of running queues start in queue order, within the queue's and the global limits.
  if (!state.killSwitch) {
    for (const q of state.queues) {
      if (!q.running) continue
      const order = [...q.order, ...jobs.filter((j) => j.queue === q.id && !q.order.includes(j.id)).map((j) => j.id)]
      for (const id of order) {
        const inQueue = jobs.filter((j) => j.queue === q.id && isActive(j)).length
        if (inQueue >= q.maxConcurrent || activeCount() >= s.concurrentJobs) break
        const i = jobs.findIndex((j) => j.id === id && j.state === 'queued')
        if (i < 0) continue
        jobs[i] = note({ ...(jobs[i] as Job), state: 'probing', probeUntil: now + 900 }, `Started by ${q.name}`)
      }
    }
  }

  const downloading = jobs.filter((j) => j.state === 'downloading')
  const lim = state.limiter
  jobs = jobs.map((j0) => {
    let j = j0
    switch (j.state) {
      case 'probing': {
        if ((j.probeUntil ?? 0) > now) return j
        if (j.sessionOrigin && state.settings.siteLogins.some((l) => l.origin === j.sessionOrigin && l.scope === 'never')) {
          return note({ ...j, state: 'failed', error: { code: 'policy.never', message: 'You blocked this site.', next: 'Remove the block in Settings, Site logins to ask again.' } }, 'Refused: the site is on your never list')
        }
        if (j.sessionOrigin && !state.settings.siteLogins.some((l) => l.origin === j.sessionOrigin)) {
          needApproval.push(j)
          return note({ ...j, state: 'awaiting_session' }, 'The site needs a login; asking you for permission')
        }
        if (j.sessionOrigin && state.settings.siteLogins.some((l) => l.origin === j.sessionOrigin && l.scope === 'once')) {
          state = { ...state, settings: { ...state.settings, siteLogins: state.settings.siteLogins.filter((l) => !(l.origin === j.sessionOrigin && l.scope === 'once')) } }
        }
        return note({ ...j, state: 'downloading', resumable: 'verified', segments: j.segments.length ? j.segments.map((x) => (x.state === 'done' ? x : { ...x, state: 'active' })) : makeSegments(j.size, j.connections, j.done, true) }, j.done > 0 ? `Resumed at ${fmtBytes(j.done)}: the server's validators matched` : 'Probe: 206 Partial Content, range support verified')
      }
      case 'downloading': {
        const size = j.size ?? 0
        const q = state.queues.find((x) => x.id === j.queue)
        const qActive = q ? downloading.filter((x) => x.queue === q.id).length : 0
        const capacity = (j.script?.maxRate ?? 3 * MiB) * (0.8 + 0.4 * Math.abs(Math.sin(now / 2300 + j.added)))
        const cap = jobRateCap({ capacity, limiter: lim, now, activeJobs: downloading.length, queueLimitKiBps: q?.limitKiBps ?? null, queueActiveJobs: qActive })
        const rate = j.rate ? j.rate * 0.6 + cap * 0.4 : cap
        let add = rate * dt
        const segments = j.segments.map((x) => ({ ...x }))
        const open = segments.filter((x) => x.state !== 'done')
        const per = open.length ? add / open.length : 0
        for (const seg of open) {
          const room = seg.end - seg.start - seg.done
          const got = Math.min(room, per)
          seg.done += got
          seg.state = seg.done >= seg.end - seg.start - 0.5 ? 'done' : 'active'
          add -= got
        }
        const done = Math.min(size, segments.reduce((n, x) => n + x.done, 0))
        j = { ...j, rate, segments, done }
        const frac = size ? done / size : 0
        if (j.script?.failAt !== undefined && frac >= j.script.failAt) {
          const { failAt: _f, ...script } = j.script
          const failed = note(
            stopSegments({ ...j, script, state: 'failed', error: { code: 'http.url_expired', message: 'The link expired: the server answered 403 to a signed address that worked before.', next: 'Refresh the address from the website, then retry. Saved bytes are kept.' } }),
            `Failed at ${fmtBytes(done)}: the signed link expired (403)`,
          )
          finished.push(failed)
          return failed
        }
        if (j.script?.retryAt !== undefined && frac >= j.script.retryAt) {
          const { retryAt: _r, ...script } = j.script
          return note(stopSegments({ ...j, script, state: 'retry_scheduled', retryAt: now + 25_000, error: { code: 'http.unavailable', message: 'The server is busy (503) and asked Zeus to wait.', next: 'Zeus retries on its own in 25 seconds. Nothing to do.' } }), 'The server answered 503 with Retry-After: 25; retry scheduled')
        }
        if (size && done >= size - 0.5) return note({ ...j, done: size, rate: verifyRate(), state: 'verifying', hashed: 0, segments: segments.map((x) => ({ ...x, done: x.end - x.start, state: 'done' })) }, 'All bytes committed; verifying')
        return j
      }
      case 'retry_scheduled': {
        if ((j.retryAt ?? 0) > now) return j
        const { error: _e, retryAt: _r, ...rest } = j
        return note({ ...rest, state: 'probing', probeUntil: now + 600 }, 'Retry: probing again')
      }
      case 'verifying': {
        const hashed = Math.min(j.size ?? 0, (j.hashed ?? 0) + verifyRate() * dt)
        if (hashed >= (j.size ?? 0)) return { ...j, hashed, state: 'finalizing', rate: 0 }
        return { ...j, hashed }
      }
      case 'finalizing': {
        const sha = j.expectedSha256 ?? sha256Hint(j)
        const exe = j.category === 'programs' || /\.(exe|msi|bat|cmd|ps1|scr|jar|lnk)$/i.test(j.name)
        const done: Job = note({ ...j, state: 'completed', finished: now, rate: 0, sha256: sha, blake3: sha256Hint({ ...j, name: 'b3' + j.name }), ...(j.expectedSha256 ? { matchedExpected: true } : {}), ...(exe ? { quarantined: true, quarantineReasons: ['executable_type'] } : {}) }, exe ? 'Completed; quarantined: executable type. Zeus never opens or runs it.' : 'Completed and verified; provenance receipt written')
        finished.push(done)
        return done
      }
      default:
        return j
    }
  })

  // Queues that ran out of work finish and run their on-finish action.
  let queues = state.queues
  for (const q of queues) {
    if (!q.running || q.kind === 'sync') continue
    const left = jobs.some((j) => j.queue === q.id && (j.state === 'queued' || isActive(j) || j.state === 'retry_scheduled'))
    if (!left) {
      queues = queues.map((x) => (x.id === q.id ? { ...x, running: false } : x))
      const action = q.onFinish === 'notify' ? 'Notified you' : q.onFinish === 'stop_limiter' ? 'Turned the speed limiter off' : q.onFinish === 'close_app' ? 'Would close the window (not in this release)' : q.onFinish === 'sleep' ? 'Would put the computer to sleep (not in this release)' : 'Nothing else to do'
      if (q.onFinish === 'stop_limiter') state = { ...state, limiter: { ...state.limiter, enabled: false } }
      toast({ kind: 'ok', title: `${q.name} finished`, desc: action, important: true, topic: 'queue' })
    }
  }

  // Approvals past their deadline expire; their jobs stop waiting.
  let approvals = state.approvals
  if (approvals.some((a) => a.state === 'pending' && Date.parse(a.facts.expires_at) <= now)) {
    approvals = approvals.map((a) => (a.state === 'pending' && Date.parse(a.facts.expires_at) <= now ? { ...a, state: 'expired' } : a))
  }

  // The index scan.
  let index = state.index
  if (index.state === 'scanning') {
    const progress = Math.min(1, index.progress + dt / 4)
    index = progress >= 1 ? { state: 'current', progress: 1, generation: index.generation + 1, lastScan: now } : { ...index, progress }
  }

  let files = state.files
  for (const f of finished) {
    if (f.state !== 'completed') continue
    files = [...files.filter((x) => x.jobId !== f.id), fileOfJob(f, files.length + 100000)]
  }
  state = { ...state, jobs, queues, approvals, index, files }
  // A job that needs a site login it has no permission for raises a session_grant approval; jobs of the same
  // requester and site join one pending request as new revisions (spec 07 section 4.7).
  for (const j of needApproval) {
    const origin = j.sessionOrigin as string
    const pending = state.approvals.find((a) => a.state === 'pending' && a.facts.kind === 'session_grant' && a.facts.website === origin && a.facts.requester.label === j.source.label)
    if (pending) {
      set({ jobs: state.jobs.map((x) => (x.id === j.id ? { ...x, state: 'awaiting_approval', approvalId: pending.id } : x)) })
      reviseApproval(pending.id, { addFile: { name: j.name, size: j.size }, jobId: j.id })
    } else {
      createApproval({ kind: 'session_grant', website: origin, requester: { kind: j.source.kind === 'agent' ? 'agent' : 'human', label: j.source.label }, files: [{ name: j.name, size: j.size }], destination: j.dir, jobIds: [j.id] })
    }
  }
  for (const f of finished) {
    if (f.state === 'completed') toast({ kind: f.quarantined ? 'warn' : 'ok', title: f.quarantined ? 'Download complete, quarantined' : 'Download complete', desc: `${f.name} · ${fmtBytes(f.size)}`, important: false, topic: 'complete', to: `#/downloads?job=${f.id}`, sound: 'done' })
    else if (f.state === 'failed') toast({ kind: 'bad', title: 'Download failed', desc: `${f.name}: ${f.error?.message ?? ''}`, important: true, topic: 'failure', to: `#/downloads?job=${f.id}`, sound: 'fail' })
  }
  emit()
}

function sha256Hint(j: Pick<Job, 'name' | 'size' | 'url'>): string {
  // A stand-in digest for sample data, stable per file.
  return requestHash({ name: j.name, size: Math.round(j.size ?? 0), url: j.url })
}

let timer: ReturnType<typeof setInterval> | undefined

export function startClock(): void {
  if (timer !== undefined) return
  if (LIVE) startLive({ get: getState, set, toast, subscribe, defaultDir, queueName })
  timer = setInterval(tick, LIVE ? 1000 : TICK_MS)
}

export const SAMPLE_ROOTS = [ROOT, ARCHIVE]

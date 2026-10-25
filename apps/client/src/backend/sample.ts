// The sample state this release starts from: neutral file names on reserved example domains, a few queues, a file
// index over two download roots, and two finished agent threads. Generated from a fixed seed so every start looks the
// same. Nothing here describes a real person, organisation or file.

import { categoryOf, defaultMap } from '../model/categories.ts'
import { sha256Hex } from '../model/approvals.ts'
import type { FileEntry, Job, JobSource, JobState, Queue, Segment, SpeedLimiter } from '../model/types.ts'
import { pick, rng, ulid } from './rand.ts'

const MiB = 1024 * 1024
const GiB = 1024 * MiB
const MIN = 60_000
const HOUR = 3_600_000
const DAY = 86_400_000

export const ROOT = 'D:\\Downloads\\Zeus'
export const ARCHIVE = 'E:\\Archive\\Zeus'

const FOLDER: Record<string, string> = { compressed: 'Compressed', documents: 'Documents', music: 'Music', programs: 'Programs', video: 'Video', pictures: 'Pictures', general: 'General' }

export const YOU: JobSource = { kind: 'app', label: 'You, in Zeus' }
export const CLAUDE: JobSource = { kind: 'agent', label: 'Claude Code (MCP)' }
export const CURSOR: JobSource = { kind: 'agent', label: 'Cursor (MCP)' }
export const EDGE: JobSource = { kind: 'browser_handoff', label: 'Edge, browser handoff' }
export const CLI: JobSource = { kind: 'cli', label: 'zeus get, terminal' }
export const WORKFLOW: JobSource = { kind: 'workflow', label: 'Workflow run' }

/** Splits a size into `n` contiguous segments and spreads `done` bytes over them, front segments first. */
export function makeSegments(size: number | null, n: number, done: number, active: boolean): Segment[] {
  if (!size || size <= 0) return []
  const count = Math.max(1, Math.min(n, Math.ceil(size / (2 * MiB))))
  const len = Math.ceil(size / count)
  const segs: Segment[] = []
  // Each segment has made similar progress, the way parallel connections do.
  const frac = Math.min(1, done / size)
  for (let i = 0; i < count; i++) {
    const start = i * len
    const end = Math.min(size, start + len)
    const span = end - start
    const jitter = count > 1 ? (((i * 37) % 11) - 5) / 100 : 0
    const f = frac >= 1 ? 1 : Math.max(0, Math.min(1, frac + jitter))
    const segDone = Math.round(span * f)
    segs.push({ start, end, done: segDone, state: segDone >= span ? 'done' : active ? 'active' : 'pending' })
  }
  // Make the total exact.
  const total = segs.reduce((a, s) => a + s.done, 0)
  const last = segs[segs.length - 1] as Segment
  last.done = Math.max(0, Math.min(last.end - last.start, last.done + (Math.round(done) - total)))
  last.state = last.done >= last.end - last.start ? 'done' : active ? 'active' : 'pending'
  return segs
}

interface Spec {
  name: string
  size: number | null
  host: string
  path?: string
  state: JobState
  done?: number
  source?: JobSource
  queue?: string | null
  addedAgo: number
  finishedAgo?: number
  connections?: number
  rate?: number
  query?: boolean
  dir?: string
  expected?: boolean
  quarantine?: string[]
  error?: Job['error']
  script?: Job['script']
  project?: string
  description?: string
  priority?: Job['priority']
}

export function makeJob(s: Spec, now: number, r: () => number): Job {
  const category = categoryOf(s.name, defaultMap())
  const size = s.size === null ? null : Math.round(s.size)
  const done = s.state === 'completed' ? (size ?? 0) : Math.round(s.done ?? 0)
  const active = s.state === 'downloading'
  const id = ulid('job', now - s.addedAgo, r)
  const path = s.path ?? 'files'
  const sha = sha256Hex(`${s.name}:${size}`)
  const job: Job = {
    id,
    name: s.name,
    url: `https://${s.host}/${path}/${encodeURIComponent(s.name)}`,
    host: s.host,
    queryWithheld: !!s.query,
    size,
    done,
    rate: active ? (s.rate ?? 2 * MiB) : 0,
    state: s.state,
    category,
    queue: s.queue ?? null,
    added: now - s.addedAgo,
    dir: s.dir ?? `${ROOT}\\${FOLDER[category] ?? 'General'}`,
    source: s.source ?? YOU,
    segments: makeSegments(size, s.connections ?? 8, done, active),
    connections: s.connections ?? 8,
    resumable: 'verified',
    priority: s.priority ?? 'normal',
    events: [{ t: now - s.addedAgo, text: `Added by ${(s.source ?? YOU).label}` }],
  }
  if (s.finishedAgo !== undefined) job.finished = now - s.finishedAgo
  if (s.state === 'completed') {
    job.sha256 = sha
    job.blake3 = sha256Hex(`b3:${sha}`)
    if (s.expected) {
      job.expectedSha256 = sha
      job.matchedExpected = true
    }
    job.events.push({ t: job.finished ?? now, text: 'Verified: SHA-256 and BLAKE3 recorded; provenance receipt written' })
  }
  if (s.quarantine) {
    job.quarantined = true
    job.quarantineReasons = s.quarantine
  }
  if (s.error) job.error = s.error
  if (s.script) job.script = s.script
  if (s.project) job.grabberProject = s.project
  if (s.description) job.description = s.description
  return job
}

export function sampleQueues(now: number): Queue[] {
  const start = new Date(now + 2 * MIN)
  const hm = `${String(start.getHours()).padStart(2, '0')}:${String(start.getMinutes()).padStart(2, '0')}`
  return [
    {
      id: 'main',
      name: 'Main download queue',
      kind: 'download',
      builtin: true,
      running: false,
      maxConcurrent: 3,
      retries: 5,
      schedule: { startEnabled: false, startTime: '09:00', repeat: 'days', startDate: '', days: [1, 2, 3, 4, 5], stopEnabled: false, stopTime: '18:00' },
      onFinish: 'notify',
      limitKiBps: null,
      syncEveryHours: 24,
      order: [],
    },
    {
      id: 'offpeak',
      name: 'Off-peak queue',
      kind: 'download',
      builtin: false,
      running: false,
      maxConcurrent: 2,
      retries: 10,
      schedule: { startEnabled: true, startTime: hm, repeat: 'days', startDate: '', days: [0, 1, 2, 3, 4, 5, 6], stopEnabled: true, stopTime: '07:00' },
      onFinish: 'stop_limiter',
      limitKiBps: 6144,
      syncEveryHours: 24,
      order: [],
    },
    {
      id: 'sync',
      name: 'Synchronization queue',
      kind: 'sync',
      builtin: true,
      running: false,
      maxConcurrent: 1,
      retries: 3,
      schedule: { startEnabled: true, startTime: '03:00', repeat: 'days', startDate: '', days: [0, 3], stopEnabled: false, stopTime: '06:00' },
      onFinish: 'nothing',
      limitKiBps: 1024,
      syncEveryHours: 72,
      order: [],
      lastStarted: now - 2 * DAY - 9 * HOUR,
    },
  ]
}

export function sampleLimiter(): SpeedLimiter {
  return { enabled: false, limitKiBps: 4096, scheduleEnabled: true, from: '09:00', to: '18:00', days: [1, 2, 3, 4, 5] }
}

export function sampleJobs(now: number): Job[] {
  const r = rng(20261025)
  const specs: Spec[] = [
    { name: 'open-dataset-2026-10.tar.zst', size: 4.2 * GiB, host: 'data.example.org', path: 'releases/2026-10', state: 'downloading', done: 1.71 * GiB, source: CLAUDE, addedAgo: 14 * MIN, rate: 9.4 * MiB, connections: 8, script: { maxRate: 11 * MiB }, description: 'Monthly open dataset snapshot' },
    { name: 'lecture-07-signal-processing.mp4', size: 1.31 * GiB, host: 'media.example.edu', path: 'courses/dsp/video', state: 'downloading', done: 0.52 * GiB, source: EDGE, addedAgo: 6 * MIN, rate: 4.1 * MiB, connections: 6, query: true, script: { maxRate: 5.2 * MiB } },
    { name: 'maps-region-tiles.zip', size: 2.1 * GiB, host: 'cdn.example.com', path: 'tiles/v9', state: 'downloading', done: 0.61 * GiB, source: CURSOR, addedAgo: 9 * MIN, rate: 6.8 * MiB, connections: 8, query: true, script: { failAt: 0.33, maxRate: 7.5 * MiB } },
    { name: 'podcast-episode-112.mp3', size: 96 * MiB, host: 'audio.example.net', path: 'feed/episodes', state: 'downloading', done: 31 * MiB, source: YOU, addedAgo: 3 * MIN, rate: 1.4 * MiB, connections: 4, script: { retryAt: 0.4, maxRate: 1.6 * MiB } },
    { name: 'field-recordings-vol2.flac', size: 640 * MiB, host: 'archive.example.org', path: 'audio/field', state: 'paused', done: 262 * MiB, source: YOU, addedAgo: 2 * HOUR },
    { name: 'survey-results-q3.xlsx', size: 2.4 * MiB, host: 'files.example.net', path: 'shared/reports', state: 'completed', source: CLAUDE, addedAgo: 50 * MIN, finishedAgo: 49 * MIN },
    { name: 'toolkit-setup-3.2.1.exe', size: 88 * MiB, host: 'releases.example.com', path: 'toolkit/3.2.1', state: 'completed', source: CLI, addedAgo: 3 * HOUR, finishedAgo: 3 * HOUR - 2 * MIN, quarantine: ['executable_type'], expected: true },
    { name: 'annual-report-2025.pdf', size: 12.6 * MiB, host: 'papers.example.org', path: 'reports', state: 'completed', source: YOU, addedAgo: 5 * HOUR, finishedAgo: 5 * HOUR - MIN, expected: true },
    { name: 'mirror-list.txt', size: 18 * 1024, host: 'mirror.example.net', path: '', state: 'completed', source: CLI, addedAgo: 26 * HOUR, finishedAgo: 26 * HOUR },
    { name: 'style-guide-v4.pdf', size: 34 * MiB, host: 'docs.example.org', path: 'design', state: 'queued', queue: 'main', source: YOU, addedAgo: 40 * MIN },
    { name: 'open-font-pack.zip', size: 212 * MiB, host: 'cdn.example.com', path: 'fonts', state: 'queued', queue: 'main', source: YOU, addedAgo: 39 * MIN },
    { name: 'satellite-imagery-2026-09.tif', size: 3.1 * GiB, host: 'data.example.org', path: 'imagery', state: 'queued', queue: 'offpeak', source: CLAUDE, addedAgo: 2 * HOUR },
    { name: 'audio-archive-part1.7z', size: 1.8 * GiB, host: 'archive.example.org', path: 'bulk', state: 'queued', queue: 'offpeak', source: YOU, addedAgo: 2 * HOUR },
    { name: 'audio-archive-part2.7z', size: 1.7 * GiB, host: 'archive.example.org', path: 'bulk', state: 'queued', queue: 'offpeak', source: YOU, addedAgo: 2 * HOUR },
    { name: 'training-videos-batch.zip', size: 5.6 * GiB, host: 'media.example.edu', path: 'training', state: 'queued', queue: 'offpeak', source: YOU, addedAgo: 2 * HOUR - MIN },
    { name: 'weekly-digest.epub', size: 3.3 * MiB, host: 'news.example.org', path: 'digest', state: 'completed', queue: 'sync', source: YOU, addedAgo: 9 * DAY, finishedAgo: 2 * DAY },
    { name: 'station-schedule.csv', size: 410 * 1024, host: 'data.example.org', path: 'transit', state: 'completed', queue: 'sync', source: YOU, addedAgo: 12 * DAY, finishedAgo: 2 * DAY },
    { name: 'old-installer-1.0.msi', size: 41 * MiB, host: 'releases.example.com', path: 'legacy', state: 'canceled', source: YOU, addedAgo: 2 * DAY },
    { name: 'mobile-app-build.apk', size: 57 * MiB, host: 'releases.example.com', path: 'android', state: 'failed', done: 12 * MiB, source: CURSOR, addedAgo: 30 * HOUR, error: { code: 'http.not_found', message: 'The server no longer has this file (404).', next: 'Check the release page for a newer address, then add it again.' } },
    { name: 'city-photos-2026.zip', size: 780 * MiB, host: 'photos.example.net', path: 'albums', state: 'completed', source: EDGE, addedAgo: 2 * DAY, finishedAgo: 2 * DAY - 9 * MIN },
    { name: 'thesis-template.docx', size: 1.1 * MiB, host: 'portal.example.edu', path: 'templates', state: 'completed', source: YOU, addedAgo: 3 * DAY, finishedAgo: 3 * DAY },
    { name: 'concert-live-set.mkv', size: 2.9 * GiB, host: 'media.example.edu', path: 'events', state: 'completed', source: EDGE, addedAgo: 4 * DAY, finishedAgo: 4 * DAY - 22 * MIN },
    { name: 'sensor-readings-2026-q3.json', size: 64 * MiB, host: 'data.example.org', path: 'sensors', state: 'completed', source: CLAUDE, addedAgo: 5 * DAY, finishedAgo: 5 * DAY, expected: true },
    { name: 'synth-samples.wav', size: 120 * MiB, host: 'audio.example.net', path: 'samples', state: 'completed', source: YOU, addedAgo: 6 * DAY, finishedAgo: 6 * DAY },
    { name: 'brand-assets.zip', size: 46 * MiB, host: 'docs.example.org', path: 'site-grab', state: 'completed', source: WORKFLOW, addedAgo: 8 * DAY, finishedAgo: 8 * DAY, project: 'docs.example.org' },
    { name: 'getting-started.pdf', size: 2.2 * MiB, host: 'docs.example.org', path: 'site-grab', state: 'completed', source: WORKFLOW, addedAgo: 8 * DAY, finishedAgo: 8 * DAY, project: 'docs.example.org' },
    { name: 'api-reference.pdf', size: 5.9 * MiB, host: 'docs.example.org', path: 'site-grab', state: 'completed', source: WORKFLOW, addedAgo: 8 * DAY, finishedAgo: 8 * DAY, project: 'docs.example.org' },
    { name: 'changelog-2026.md', size: 88 * 1024, host: 'docs.example.org', path: 'site-grab', state: 'completed', source: WORKFLOW, addedAgo: 8 * DAY, finishedAgo: 8 * DAY, project: 'docs.example.org' },
  ]
  return specs.map((s) => makeJob(s, now, r))
}

// ---------------------------------------------------------------- the file index

const WORDS = ['annual', 'report', 'survey', 'notes', 'draft', 'final', 'dataset', 'lecture', 'session', 'meeting', 'archive', 'backup', 'invoice', 'receipt', 'manual', 'guide', 'reference', 'summary', 'slides', 'chapter', 'appendix', 'sample', 'export', 'snapshot', 'release', 'build', 'setup', 'portable', 'tiles', 'imagery', 'recording', 'interview', 'podcast', 'episode', 'mix', 'live', 'studio', 'field', 'birds', 'harbour', 'city', 'trail', 'garden', 'kitchen', 'budget', 'plan', 'roadmap', 'catalog', 'inventory', 'schedule', 'readings', 'metrics', 'weekly', 'monthly', 'quarterly', 'research', 'paper', 'thesis', 'poster', 'brochure', 'template', 'fonts', 'icons', 'wallpapers', 'scans', 'photos', 'raw', 'edit', 'cut', 'trailer', 'tutorial', 'workshop', 'course', 'module', 'unit', 'lab', 'exercise', 'solutions', 'answers']

const EXT_BY_FOLDER: Record<string, string[]> = {
  Compressed: ['zip', 'zip', '7z', 'tar.gz', 'rar', 'zst', 'iso'],
  Documents: ['pdf', 'pdf', 'pdf', 'docx', 'xlsx', 'pptx', 'epub', 'txt', 'md', 'csv', 'json'],
  Music: ['mp3', 'mp3', 'flac', 'wav', 'ogg', 'm4a'],
  Programs: ['exe', 'msi', 'zip', 'appimage', 'deb', 'dmg', 'apk'],
  Video: ['mp4', 'mp4', 'mkv', 'webm', 'mov'],
  Pictures: ['jpg', 'jpg', 'png', 'webp', 'heic', 'svg', 'tif'],
  General: ['bin', 'dat', 'log', 'ics', 'torrent', 'srt'],
}

const SUBFOLDERS: Record<string, string[]> = {
  Compressed: ['Datasets', 'Backups', 'Fonts'],
  Documents: ['Reports 2025', 'Reports 2026', 'Papers', 'Manuals', 'Course notes'],
  Music: ['Podcasts', 'Field recordings', 'Samples'],
  Programs: ['Installers', 'Portable'],
  Video: ['Lectures', 'Events', 'Tutorials'],
  Pictures: ['Scans', 'Wallpapers', 'City photos'],
  General: ['Misc'],
}

const SIZE_BY_FOLDER: Record<string, [number, number]> = {
  Compressed: [2 * MiB, 6 * GiB],
  Documents: [20 * 1024, 60 * MiB],
  Music: [3 * MiB, 400 * MiB],
  Programs: [1 * MiB, 900 * MiB],
  Video: [40 * MiB, 8 * GiB],
  Pictures: [80 * 1024, 40 * MiB],
  General: [1024, 20 * MiB],
}

const HOSTS = ['data.example.org', 'media.example.edu', 'files.example.net', 'releases.example.com', 'papers.example.org', 'cdn.example.com', 'mirror.example.net', 'archive.example.org', 'audio.example.net', 'photos.example.net', 'docs.example.org', 'portal.example.edu']

/** The index over the download roots: folders and files with sizes and dates, plus the files of finished jobs. */
export function sampleIndex(now: number, jobs: Job[]): FileEntry[] {
  const r = rng(424242)
  const out: FileEntry[] = []
  let id = 1
  const folder = (dir: string, name: string, modified: number) => out.push({ id: id++, name, dir, isFolder: true, size: 0, modified })
  for (const root of [ROOT, ARCHIVE]) {
    const share = root === ROOT ? 1 : 0.35
    for (const top of Object.keys(EXT_BY_FOLDER)) {
      const topDir = `${root}\\${top}`
      folder(root, top, now - Math.floor(r() * 30) * DAY)
      const subs = SUBFOLDERS[top] ?? []
      for (const sub of subs) folder(topDir, sub, now - Math.floor(r() * 90) * DAY)
      const n = Math.round((top === 'Documents' ? 240 : top === 'Pictures' ? 200 : top === 'Music' ? 140 : 100) * share)
      for (let i = 0; i < n; i++) {
        const dir = r() < 0.55 && subs.length ? `${topDir}\\${pick(r, subs)}` : topDir
        const words = [pick(r, WORDS), pick(r, WORDS)]
        if (r() < 0.5) words.push(String(2019 + Math.floor(r() * 8)))
        if (r() < 0.25) words.push(`v${1 + Math.floor(r() * 5)}`)
        const sep = r() < 0.6 ? '-' : r() < 0.5 ? '_' : ' '
        let base = words.join(sep)
        if (r() < 0.15) base = base.charAt(0).toUpperCase() + base.slice(1)
        const ext = pick(r, EXT_BY_FOLDER[top] ?? ['bin'])
        const [lo, hi] = SIZE_BY_FOLDER[top] ?? [1024, MiB]
        const size = Math.round(lo * Math.pow(hi / lo, r() * r()))
        const age = Math.floor(Math.pow(r(), 2) * 420 * DAY)
        const host = pick(r, HOSTS)
        const name = `${base}.${ext}`
        out.push({ id: id++, name, dir, isFolder: false, size, modified: now - age, host, sha256: sha256Hex(`${dir}\\${name}`), ...(top === 'Programs' && /\.(exe|msi)$/.test(name) && r() < 0.5 ? { quarantined: true } : {}) })
      }
    }
  }
  for (const j of jobs) if (j.state === 'completed') out.push(fileOfJob(j, id++))
  return out
}

export function fileOfJob(j: Job, id: number): FileEntry {
  return {
    id,
    name: j.name,
    dir: j.dir,
    isFolder: false,
    size: j.size ?? j.done,
    modified: j.finished ?? Date.now(),
    jobId: j.id,
    host: j.host,
    ...(j.sha256 ? { sha256: j.sha256 } : {}),
    ...(j.quarantined ? { quarantined: true } : {}),
  }
}

export const SAMPLE_HOSTS = HOSTS
export const UNITS = { MiB, GiB, MIN, HOUR, DAY }

// The file index of the desktop app: what the page reads (a delta of the engine's index every second or two), the
// preview of one file, and the file operations the person clicks. Nothing here touches a disk: every call goes through
// the closed route table, whose Rust side checks each path against download and explicitly managed folders.

import type { Settings } from './settings.ts'
import { call } from './remote.ts'

export interface IndexRow {
  path: string
  size: number
  mtime: number
  is_dir: boolean
}

export interface IndexRootStatus {
  path: string
  include: boolean
  download: boolean
  monitor_changes: boolean
  rescan_every_hours: number | null
  entries: number
  last_scan: number | null
  exists: boolean
}

export interface IndexStatus {
  state: 'scanning' | 'current'
  progress: number
  scanning: string
  verifying: boolean
  entries: number
  generation: number
  last_scan: number | null
  watcher: boolean
  capped: boolean
  cap: number
  roots: IndexRootStatus[]
}

export interface IndexDelta extends IndexStatus {
  reset: boolean
  files: IndexRow[]
  upserts: IndexRow[]
  removes: string[]
}

export interface IndexConfigWire {
  roots: { path: string; include: boolean; monitor_changes: boolean; rescan_every_hours: number | null; download: boolean; manage_files?: boolean }[]
  exclusions: string[]
  exclude_hidden: boolean
  exclude_system: boolean
}

export interface Preview {
  kind: 'text' | 'image' | 'meta' | 'folder' | 'refused'
  path?: string
  name?: string | null
  size?: number
  modified_ms?: number
  mime?: string
  text?: string
  truncated?: boolean
  lines?: number | null
  note?: string | null
  data_url?: string
  width?: number
  height?: number
  reason?: string
  message?: string
  entries?: number
  bytes?: number
  quarantined?: boolean
  job_id?: string | null
  sha256?: string | null
  blake3?: string | null
}

export interface DeleteResult {
  ok: boolean
  trashed: string[]
  deleted: string[]
  failed: { path: string; code: string; message: string }[]
}

export const filesApi = {
  changes: (since: number) => call<IndexDelta>('POST', '/native/v1/index/changes', { since }),
  status: () => call<IndexStatus>('GET', '/native/v1/index/status'),
  rescan: (path?: string) => call<unknown>('POST', '/svc/v1/index/rescan', path ? { path } : {}),
  config: () => call<IndexConfigWire>('GET', '/native/v1/index/config'),
  putConfig: (c: IndexConfigWire) => call<IndexConfigWire>('PUT', '/native/v1/index/config', c),
  preview: (path: string) => call<Preview>('POST', '/native/v1/files/preview', { path }),
  open: (path: string, action: 'open' | 'reveal') => call<{ ok: boolean }>('POST', '/native/v1/files/open', { path, action }),
  rename: (path: string, name: string) => call<{ ok: boolean; from: string; path: string }>('POST', '/native/v1/files/rename', { path, name }),
  copy: (path: string, dir: string) => call<{ ok: boolean; path: string }>('POST', '/native/v1/files/copy', { path, dir }),
  mkdir: (parent: string, name: string) => call<{ ok: boolean; path: string }>('POST', '/native/v1/files/mkdir', { parent, name }),
  move: (path: string, dir: string) => call<{ ok: boolean; from: string; path: string }>('POST', '/native/v1/files/move', { path, dir }),
  remove: (paths: string[], permanent = false) => call<DeleteResult>('POST', '/native/v1/files/delete', { paths, permanent }),
}

export interface IndexHooks {
  /** Apply a delta (or the whole list) to the page's copy; returns nothing. */
  apply: (d: IndexDelta) => void
  /** True while the page is hidden: the loop slows down. */
  hidden?: () => boolean
}

let timer: ReturnType<typeof setTimeout> | undefined
let since = 0
let hooks: IndexHooks | undefined
let started = false

async function tick(): Promise<void> {
  if (!hooks) return
  try {
    const d = await filesApi.changes(since)
    since = d.generation
    hooks.apply(d)
  } catch {
    // The engine may be starting; the next tick asks again.
  }
  schedule(hooks.hidden?.() ? 6000 : 1500)
}

function schedule(ms: number): void {
  if (timer !== undefined) clearTimeout(timer)
  timer = setTimeout(() => void tick(), ms)
}

/** Follow the engine's index: one cheap call every 1.5 s (6 s while the window is hidden). Safe to call twice. */
export function startIndexSync(h: IndexHooks): void {
  hooks = h
  if (started) return
  started = true
  void tick()
}

/** Ask for the next look now (after a file operation or a rescan). */
export function nudgeIndexSync(): void {
  if (started) schedule(150)
}

type IndexSettingsView = Settings['index']

/** The page's Settings, Search and index section from the engine's configuration. */
export function indexFromWire(c: IndexConfigWire, base: IndexSettingsView): IndexSettingsView {
  return {
    ...base,
    roots: c.roots.map((r) => ({ path: r.path, kind: 'folder' as const, include: r.include, monitorChanges: r.monitor_changes, rescanEveryHours: r.rescan_every_hours, download: r.download, manageFiles: r.manage_files ?? false })),
    exclusions: c.exclusions,
    excludeHidden: c.exclude_hidden,
    excludeSystem: c.exclude_system,
  }
}

/** The engine's configuration from the page's section. */
export function indexToWire(ix: IndexSettingsView): IndexConfigWire {
  return {
    roots: ix.roots.map((r) => ({ path: r.path, include: r.include, monitor_changes: r.monitorChanges, rescan_every_hours: r.rescanEveryHours, download: !!r.download, manage_files: !!r.manageFiles })),
    exclusions: ix.exclusions,
    exclude_hidden: ix.excludeHidden,
    exclude_system: ix.excludeSystem,
  }
}

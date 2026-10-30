// Native file workspace: bounded Rust search pages, scoped live folder/Everything views, metadata-only
// index status sync, previews and the durable file/archive operations the person clicks. Nothing here touches a disk: every call goes through
// the closed route table, whose Rust side checks each path against download and explicitly managed folders.

import type { Settings } from './settings.ts'
import type { Query } from '../model/search.ts'
import { call } from './remote.ts'

export interface IndexRow {
  path: string
  size: number
  mtime: number
  is_dir: boolean
  quarantined?: boolean
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
  freshness?: 'reconciling' | 'scan_incomplete' | 'watching' | 'periodic_scan'
  watcher_gaps?: number
  unreadable_directories?: number
  unavailable_roots?: number
  snapshot_error?: boolean
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

export interface CopyJob {
  id: string
  state: 'scanning' | 'copying' | 'verifying' | 'publishing' | 'interrupted' | 'complete' | 'failed' | 'cancelled'
  path: string
  bytes: number
  total_bytes: number
  files: number
  error: string
  source: string
  staging: string
  can_resume: boolean
  persistent: boolean
}

export interface FolderComparison {
  id: string
  left: string
  right: string
  content: boolean
  state: 'running' | 'complete' | 'incomplete' | 'cancelled'
  complete: boolean
  entries: number
  hashed_bytes: number
  rows: number
  error: string
}
export interface ComparisonRow {
  relative: string
  status: 'left_only' | 'right_only' | 'type_conflict' | 'directory' | 'content_match' | 'content_different' | 'metadata_match' | 'metadata_different'
  left: { path: string; size: number; is_dir: boolean; sha256: string | null } | null
  right: { path: string; size: number; is_dir: boolean; sha256: string | null } | null
}

export interface SearchPage {
  generation: number
  files: IndexRow[]
  total: number
  total_bytes: number
  next: number | null
  elapsed_ms: number
}

export interface FileHistoryRecord {
  id: string
  from: string
  path: string
  state: 'pending' | 'complete' | 'failed' | 'undoing' | 'undone'
  created_ms: number
  can_undo: boolean
  directory: boolean
  bytes: number
  trash: boolean
}

export interface FileAnalytics {
  storage?: { measurement:'last_indexed_filesystem_allocation'; known_objects:number; observed_aliases:number; unobserved_aliases:number; unknown_identity_paths:number; inconsistent_objects:number; unique_logical_bytes_known:number; allocated_bytes_known:number; unknown_allocation_objects:number; complete:boolean }
  generation: number
  files: number
  logical_bytes: number
  measurement: 'logical_file_size'
  directories: { path: string; files: number; bytes: number }[]
  extensions: { path: string; files: number; bytes: number }[]
  duplicate_candidates: { bytes: number; files: number; paths: string[]; truncated: boolean; verified: false }[]
}

function nativeQuery(q: Query): unknown[][] {
  return q.map(group => group.map(term => {
    if (term.kind === 'regex' || term.kind === 'wild') return { kind: term.kind, neg: term.neg, path: term.path, pattern: term.re.source, match_case: !term.re.ignoreCase }
    if (term.kind === 'text') return { kind: term.kind, neg: term.neg, path: term.path, value: term.value, match_case: term.matchCase, whole_word: term.wholeWord }
    return term
  }))
}

export interface ArchiveJob {
  id: string
  kind: 'create' | 'extract'
  state: 'ready' | 'running' | 'publishing' | 'interrupted' | 'complete' | 'cancelled' | 'failed' | 'uncertain'
  source: string
  target: string
  total_bytes: number
  bytes: number
  total_entries: number
  entries: number
  error: string
  staging: string | null
  sha256: string | null
  digest_kind: 'zip_bytes_sha256' | 'tree_manifest_sha256'
}
export interface ArchivePlan {
  id: string
  kind: 'create' | 'extract'
  source: string
  target: string
  total_bytes: number
  total_entries: number
  entries: { name: string; bytes: number; directory: boolean }[]
  truncated: boolean
  state: 'ready'
}

export interface EverythingSettings {
  enabled: boolean
  helper: string
  sha256: string
  instance: string
  file_id?: unknown
}
export interface EverythingConfig {
  settings: EverythingSettings
  supported: boolean
  provider: 'everything_ipc2'
  independent_usn_backend: false
  suggested_helper: string | null
}
export interface EverythingPage {
  provider: 'everything_ipc2'
  consistency: 'live'
  files: IndexRow[]
  next: number | null
  skipped: number
  elapsed_ms: number
}

export const filesApi = {
  everythingConfig: () => call<EverythingConfig>('GET', '/native/v1/index/everything/config'),
  everythingInspect: (path: string) => call<{helper: string; sha256: string; bytes: number; modified_ms: number}>('POST', '/native/v1/index/everything/inspect', { path }),
  everythingConfigure: (settings: EverythingSettings) => call<EverythingConfig>('PUT', '/native/v1/index/everything/config', settings),
  everythingSearch: (root: string, text: string, sort = 'name', descending = false, offset = 0) => call<EverythingPage>('POST', '/native/v1/index/everything/search', { root, text, sort, descending, offset }),
  archives: () => call<{jobs: ArchiveJob[]}>('GET', '/native/v1/files/archives'),
  archivePrepare: (kind: 'create' | 'extract', source: string, directory: string, name: string) => call<ArchivePlan>('POST', '/native/v1/files/archive/prepare', { kind, source, directory, name }),
  archiveStart: (id: string) => call<ArchiveJob>('POST', '/native/v1/files/archive/start', { id }),
  archiveReconcile: (id: string) => call<{ok: boolean; sha256: string}>('POST', '/native/v1/files/archive/reconcile', { id }),
  archiveCancel: (id: string) => call<ArchiveJob>('POST', '/native/v1/files/archive/cancel', { id }),
  archiveCleanup: (id: string) => call<{ok: boolean}>('POST', '/native/v1/files/archive/cleanup', { id }),
  archiveDismiss: (id: string) => call<{ok: boolean}>('POST', '/native/v1/files/archive/dismiss', { id }),
  search: (query: Query, filter: string, sort: string, descending: boolean, offset = 0, generation?: number) => call<SearchPage>('POST', '/native/v1/index/search', { groups: nativeQuery(query), filter, sort, descending, limit: 500, offset, ...(generation !== undefined ? { generation } : {}) }),
  analytics: () => call<FileAnalytics>('GET', '/native/v1/index/analytics'),
  history: () => call<{ records: FileHistoryRecord[]; load_failed: boolean }>('GET', '/native/v1/files/history'),
  undo: (id: string) => call<{ ok: boolean }>('POST', '/native/v1/files/history/undo', { id }),
  reconcile: (id: string) => call<{ ok: boolean }>('POST', '/native/v1/files/history/reconcile', { id }),
  verifyDuplicates: (paths: string[]) => call<{ algorithm: string; bytes_read: number; groups: { sha256: string; paths: string[]; verified: true }[] }>('POST', '/native/v1/files/duplicates/verify', { paths }),
  compareStart: (left: string, right: string, content: boolean) => call<FolderComparison>('POST', '/native/v1/files/compare/start', { left, right, content }),
  compareStatus: (id: string) => call<FolderComparison>('POST', '/native/v1/files/compare/status', { id }),
  compareCancel: (id: string) => call<FolderComparison>('POST', '/native/v1/files/compare/cancel', { id }),
  comparePage: (id: string, offset = 0) => call<{ id: string; complete: boolean; items: ComparisonRow[]; next: number | null; total: number }>('POST', '/native/v1/files/compare/page', { id, offset, limit: 200 }),
  copies: () => call<{ jobs: CopyJob[]; load_error: string | null }>('GET', '/native/v1/files/copies'),
  list: (path: string, after = '', directoriesOnly = false) => call<{ path: string; files: IndexRow[]; next: string | null; skipped: number }>('POST', '/native/v1/files/list', { path, after, limit: 500, directories_only: directoriesOnly }),
  copyStart: (path: string, dir: string) => call<CopyJob>('POST', '/native/v1/files/copy/start', { path, dir }),
  copyResume: (id: string) => call<CopyJob>('POST', '/native/v1/files/copy/resume', { id }),
  copyStatus: (id: string) => call<CopyJob>('POST', '/native/v1/files/copy/status', { id }),
  copyCancel: (id: string) => call<CopyJob>('POST', '/native/v1/files/copy/cancel', { id }),
  changes: (since: number) => call<IndexDelta>('POST', '/native/v1/index/changes', { since, summary_only: true }),
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

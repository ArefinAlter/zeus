// The link grabber's pure rules: what makes two addresses the same download, what a failed online check means in plain
// words, which folder a download goes to, and how a duplicate choice maps to zeusd's overwrite policy. Tested with
// node --test (grab.test.mjs).

import { categoryOf } from './categories.ts'
import type { CategoryId } from './types.ts'

/** How a duplicate is handled: leave it, download it again under a new name, or replace the file on disk. */
export type DupeChoice = 'skip' | 'again' | 'replace'

/** zeusd's overwrite policy for a name that exists (`jobs.enqueue` `overwrite`). */
export type Overwrite = 'rename' | 'fail' | 'replace'

/** What an existing download looks like to the duplicate check. */
export interface Existing {
  id: string
  /** The address as the page shows it: scheme, host and path (a query string is never shown). */
  url: string
  name: string
  /** A job state, or `pending` for something waiting to be sent. */
  state: string
}

/** The part of an address that says which file it is: scheme, host (default ports dropped) and path, no query. */
export function urlKey(raw: string): string {
  try {
    const u = new URL(raw.replace('?…', ''))
    const port = u.port && !((u.protocol === 'http:' && u.port === '80') || (u.protocol === 'https:' && u.port === '443')) ? `:${u.port}` : ''
    const path = u.pathname.length > 1 ? u.pathname.replace(/\/+$/, '') : u.pathname
    return `${u.protocol}//${u.hostname.toLowerCase()}${port}${path}`
  } catch {
    return raw
  }
}

const GONE = new Set(['failed', 'canceled'])

/** An existing download of the same file, if any. Failed and canceled jobs do not count: nothing was kept. */
export function findDuplicate(url: string, existing: Existing[]): Existing | undefined {
  const key = urlKey(url)
  return existing.find((e) => !GONE.has(e.state) && urlKey(e.url) === key)
}

/** A short phrase for the state of the download a link duplicates. */
export function duplicateWhere(e: Pick<Existing, 'state'>): string {
  switch (e.state) {
    case 'completed':
      return 'already downloaded'
    case 'pending':
      return 'already waiting to start'
    case 'queued':
      return 'already queued'
    case 'paused':
      return 'already in the list, paused'
    default:
      return 'already downloading'
  }
}

/** The overwrite policy a duplicate choice asks for; `fallback` is the batch default. */
export function overwriteFor(choice: DupeChoice | undefined, fallback: Overwrite): Overwrite {
  if (choice === 'replace') return 'replace'
  if (choice === 'again') return 'rename'
  return fallback
}

/** One answer of the online check (the shape of `LinkCheck` in remote.ts, reduced to what the rules need). */
export interface CheckAnswer {
  check: 'online' | 'offline' | 'needs_login'
  status: number
  size?: number | null
  name?: string | null
  name_source?: string | null
  resumable?: string | null
  redirects?: number | null
  content_type?: string | null
  error_code?: string | null
  error_message?: string | null
}

/** What an answer means for a person, in one sentence. */
export function checkNote(a: CheckAnswer): string {
  if (a.check === 'needs_login') return 'Needs a site login. Zeus lends a browser login only through the extension, after you approve.'
  if (a.check === 'online') {
    const parts: string[] = []
    if (a.name && a.name_source === 'content_disposition') parts.push(`the server names it ${a.name}`)
    if (a.resumable === 'unsupported') parts.push('no byte ranges: one piece, starts over if interrupted')
    else if (a.resumable === 'verified') parts.push('resumable')
    if (a.redirects) parts.push(`${a.redirects} redirect${a.redirects === 1 ? '' : 's'}`)
    return parts.join('; ')
  }
  switch (a.status) {
    case 401:
      return 'The server wants a sign-in (401).'
    case 403:
      return 'The server refused the file (403). A signed link that expired looks like this: get a fresh link from the site.'
    case 404:
      return 'The server has no such file (404). Check the address.'
    case 410:
      return 'The file was removed from the server (410). Nothing to retry.'
    case 429:
      return 'The server says too many requests (429). Try again later.'
    case 503:
      return 'The server is busy (503) and asked Zeus to wait. Zeus retries on its own once the download starts.'
    default:
      return a.status >= 500 ? `The server failed (${a.status}).` : a.error_message ? a.error_message : `The server answered ${a.status || 'nothing'}.`
  }
}

/** True when `dir` is `root` or inside it (separator and case follow the platform: pass `ci` for Windows). */
export function insideRoot(root: string, dir: string, ci: boolean): boolean {
  const norm = (s: string) => {
    const t = s.replace(/[\\/]+$/, '').replace(/\\/g, '/')
    return ci ? t.toLowerCase() : t
  }
  const r = norm(root)
  const d = norm(dir)
  return d === r || d.startsWith(`${r}/`)
}

/** Why a folder cannot be used for a download, or null. Folders must be absolute and inside a download folder. */
export function folderProblem(dir: string, roots: string[], ci: boolean): string | null {
  if (!dir.trim()) return 'Choose a folder.'
  if (!/^[A-Za-z]:[\\/]|^\\\\|^\//.test(dir)) return 'Use a full folder path.'
  if (/(^|[\\/])\.\.([\\/]|$)/.test(dir)) return 'A folder path cannot contain "..".'
  if (!roots.some((r) => insideRoot(r, dir, ci))) return 'That folder is outside the download folders Zeus may write to. Add it in Settings, Save to.'
  return null
}

/** The save-to rules a person set. */
export interface SaveToRules {
  root: string
  perCategory: boolean
  /** An absolute folder per category that overrides the default subfolder. */
  categoryDirs?: Record<string, string>
  /** The last folder used for each category, when "remember the last folder" is on. */
  lastDirs?: Record<string, string>
  rememberLast: boolean
  writeRoots: string[]
}

const SUBFOLDER: Record<string, string> = { compressed: 'Compressed', documents: 'Documents', music: 'Music', programs: 'Programs', video: 'Video', pictures: 'Pictures' }

/**
 * The folder a file of `category` goes to. Order: the category's own folder, the last folder used for it (when asked to
 * remember), a subfolder of the default folder (when "a folder per category" is on), the default folder. A rule whose
 * folder is no longer inside a download folder is ignored rather than followed.
 */
export function folderFor(category: CategoryId, rules: SaveToRules, join: (a: string, b: string) => string, ci: boolean): string {
  const ok = (d: string | undefined): d is string => !!d && rules.writeRoots.some((r) => insideRoot(r, d, ci))
  const own = rules.categoryDirs?.[category]
  if (ok(own)) return own
  const last = rules.lastDirs?.[category]
  if (rules.rememberLast && ok(last)) return last
  const sub = SUBFOLDER[category]
  if (rules.perCategory && sub) return join(rules.root, sub)
  return rules.root
}

/** The category for a file name under the person's extension lists. */
export function categoryFor(name: string, map: Record<string, string[]>): CategoryId {
  return categoryOf(name, map)
}

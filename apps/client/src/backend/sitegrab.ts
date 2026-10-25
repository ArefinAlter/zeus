// The site grabber's preview on sample pages: a bounded walk from a start page, by depth, file types and include or
// exclude rules, listing what would be fetched before anything is. zeusd runs this as a workflow plan (spec 08) whose
// run needs a person's approval; in this release the pages are generated from the start address, not fetched.

import { extOf } from '../model/categories.ts'
import { wildcardRe } from '../model/search.ts'
import { pick, rng } from './rand.ts'

export interface SiteRequest {
  startUrl: string
  depth: number
  sameHostOnly: boolean
  types: string[]
  include: string[]
  exclude: string[]
  maxFiles: number
  maxMiB: number
}

export interface SitePreview {
  ok: boolean
  error?: string
  host: string
  pages: { url: string; depth: number }[]
  files: { url: string; name: string; size: number; depth: number; from: string; host: string }[]
  skipped: { reason: string; count: number }[]
  totalBytes: number
}

const SECTIONS = ['docs', 'guides', 'downloads', 'releases', 'papers', 'media', 'archive', 'blog', 'help']
const STEMS = ['getting-started', 'user-guide', 'reference', 'release-notes', 'whitepaper', 'overview', 'tutorial', 'quick-start', 'faq', 'changelog', 'handbook', 'spec', 'slides', 'dataset', 'sample-data', 'brochure', 'poster', 'manual']
const EXTS = ['pdf', 'pdf', 'pdf', 'pdf', 'pdf', 'zip', 'epub', 'epub', 'mp4', 'mp3', 'png', 'jpg', 'docx', 'xlsx', 'pptx', 'csv', 'exe', 'msi', 'tar.gz']

function hashString(s: string): number {
  let h = 2166136261
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619)
  return h >>> 0
}

function matchesAny(patterns: string[], path: string): boolean {
  return patterns.some((p) => {
    const t = p.trim()
    if (!t) return false
    return /[*?]/.test(t) ? wildcardRe(t, false).test(path) : path.toLowerCase().includes(t.toLowerCase())
  })
}

export function previewSite(req: SiteRequest): SitePreview {
  let start: URL
  try {
    start = new URL(req.startUrl)
  } catch {
    return { ok: false, error: 'The start page is not an address.', host: '', pages: [], files: [], skipped: [], totalBytes: 0 }
  }
  if (start.protocol !== 'https:' && start.protocol !== 'http:') return { ok: false, error: 'The start page must be an http or https address.', host: '', pages: [], files: [], skipped: [], totalBytes: 0 }
  if (start.username || start.password) return { ok: false, error: 'No user name or password in the address. Logged-in pages go through the Zeus extension.', host: '', pages: [], files: [], skipped: [], totalBytes: 0 }
  const host = start.hostname
  const r = rng(hashString(host + start.pathname))
  const pages: SitePreview['pages'] = [{ url: `${start.origin}${start.pathname}`, depth: 0 }]
  const files: SitePreview['files'] = []
  const skipped = new Map<string, number>()
  const skip = (reason: string) => skipped.set(reason, (skipped.get(reason) ?? 0) + 1)
  const base = start.pathname.replace(/\/[^/]*$/, '/')
  let frontier = [`${start.origin}${base}`]
  let totalBytes = 0
  for (let depth = 1; depth <= Math.min(req.depth, 4); depth++) {
    const next: string[] = []
    for (const page of frontier) {
      const linksOut = 2 + Math.floor(r() * 3)
      for (let i = 0; i < linksOut; i++) {
        const sec = pick(r, SECTIONS)
        const url = `${page.replace(/\/$/, '')}/${sec}/`
        if (pages.some((p) => p.url === url)) continue
        if (matchesAny(req.exclude, new URL(url).pathname)) {
          skip('page excluded by a rule')
          continue
        }
        pages.push({ url, depth })
        next.push(url)
      }
      // Off-site links are counted, never followed, when the walk stays on this host.
      const offsite = Math.floor(r() * 3)
      for (let i = 0; i < offsite; i++) skip(req.sameHostOnly ? 'link to another host' : 'another host (not in this release)')
      const fileCount = 3 + Math.floor(r() * 6)
      for (let i = 0; i < fileCount; i++) {
        const ext = pick(r, EXTS)
        const name = `${pick(r, STEMS)}${r() < 0.4 ? `-${2020 + Math.floor(r() * 7)}` : ''}.${ext}`
        const url = `${page.replace(/\/$/, '')}/${name}`
        const path = new URL(url).pathname
        const size = Math.round(40 * 1024 * Math.pow(4000, r()))
        const e = extOf(name)
        if (req.types.length && !req.types.includes(e) && !(ext === 'tar.gz' && req.types.includes('gz'))) {
          skip('file type not selected')
          continue
        }
        if (req.include.some((p) => p.trim()) && !matchesAny(req.include, path)) {
          skip('not matched by an include rule')
          continue
        }
        if (matchesAny(req.exclude, path)) {
          skip('file excluded by a rule')
          continue
        }
        if (files.some((f) => f.url === url)) {
          skip('duplicate')
          continue
        }
        if (files.length >= req.maxFiles) {
          skip('over the file limit')
          continue
        }
        if (totalBytes + size > req.maxMiB * 1024 * 1024) {
          skip('over the size limit')
          continue
        }
        totalBytes += size
        files.push({ url, name, size, depth, from: page, host })
      }
    }
    frontier = next
  }
  return { ok: true, host, pages, files, skipped: [...skipped.entries()].map(([reason, count]) => ({ reason, count })), totalBytes }
}

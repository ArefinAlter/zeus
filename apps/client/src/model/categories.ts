// File types and categories: which extensions belong to which category, and where each category saves by default.
// The person can change both in Settings, File types; the defaults follow the categories download managers have long
// used (compressed, documents, music, programs, video) plus pictures. Pure data and functions, tested with node --test.

import type { CategoryId } from './types.ts'

export interface CategoryInfo {
  id: CategoryId
  label: string
  /** Lower-case extensions without the dot. */
  exts: string[]
  /** The folder under the download root. */
  folder: string
}

export const CATEGORIES: CategoryInfo[] = [
  { id: 'compressed', label: 'Compressed', exts: ['zip', 'rar', '7z', 'tar', 'gz', 'tgz', 'bz2', 'xz', 'zst', 'iso', 'cab'], folder: 'Compressed' },
  { id: 'documents', label: 'Documents', exts: ['pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'txt', 'md', 'epub', 'csv', 'json', 'rtf'], folder: 'Documents' },
  { id: 'music', label: 'Music', exts: ['mp3', 'flac', 'wav', 'ogg', 'opus', 'm4a', 'aac', 'wma'], folder: 'Music' },
  { id: 'programs', label: 'Programs', exts: ['exe', 'msi', 'msix', 'dmg', 'pkg', 'deb', 'rpm', 'appimage', 'apk', 'jar'], folder: 'Programs' },
  { id: 'video', label: 'Video', exts: ['mp4', 'mkv', 'webm', 'mov', 'avi', 'm4v', 'wmv', 'ts'], folder: 'Video' },
  { id: 'pictures', label: 'Pictures', exts: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'tif', 'tiff', 'heic', 'raw'], folder: 'Pictures' },
  { id: 'general', label: 'General', exts: [], folder: '' },
]

/** The extension of a file name, lower case, without the dot; '' when there is none. Handles `.tar.gz`-style names. */
export function extOf(name: string): string {
  const base = name.split(/[\\/]/).pop() ?? name
  const m = /\.([A-Za-z0-9]{1,10})$/.exec(base)
  if (!m || base.startsWith('.') && base.indexOf('.', 1) === -1) return ''
  return (m[1] ?? '').toLowerCase()
}

/** The category of a file name under a mapping of category to extensions. Unknown types are `general`. */
export function categoryOf(name: string, map: Record<string, string[]> = defaultMap()): CategoryId {
  const ext = extOf(name)
  if (!ext) return 'general'
  for (const c of CATEGORIES) {
    if (c.id === 'general') continue
    if ((map[c.id] ?? []).includes(ext)) return c.id
  }
  return 'general'
}

export function defaultMap(): Record<string, string[]> {
  const out: Record<string, string[]> = {}
  for (const c of CATEGORIES) out[c.id] = [...c.exts]
  return out
}

/** Parses the extension list a person types ("zip rar, .7z") into clean, unique, lower-case entries. */
export function parseExtList(text: string): string[] {
  const out: string[] = []
  for (const raw of text.split(/[\s,;]+/)) {
    const e = raw.trim().replace(/^\*?\./, '').toLowerCase()
    if (/^[a-z0-9]{1,10}$/.test(e) && !out.includes(e)) out.push(e)
  }
  return out
}

/** Extensions that appear in more than one category: a mapping a person saves must not be ambiguous. */
export function overlaps(map: Record<string, string[]>): { ext: string; categories: string[] }[] {
  const seen = new Map<string, string[]>()
  for (const [cat, exts] of Object.entries(map)) for (const e of exts) seen.set(e, [...(seen.get(e) ?? []), cat])
  return [...seen.entries()].filter(([, c]) => c.length > 1).map(([ext, categories]) => ({ ext, categories }))
}

export function categoryLabel(id: string): string {
  return CATEGORIES.find((c) => c.id === id)?.label ?? id
}

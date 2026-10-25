// Link intake for the link grabber: pull http(s) addresses out of pasted text or a pasted page, drop exact
// duplicates, refuse what Zeus refuses, and group the rest into packages by host. The limits follow the desktop
// client's real review flow (apps/desktop/README.md): at most 50 unique http(s) URLs and 64 KiB of input, and
// different signed query strings stay different inputs. Pure functions, tested with node --test.

export const MAX_URLS = 50
export const MAX_INPUT_BYTES = 64 * 1024

export interface ParsedLink {
  /** The address exactly as given; it never leaves the review state of this page. */
  url: string
  host: string
  /** Scheme, host and path: what the screen shows (a query string is withheld, as redact_url does). */
  display: string
  queryWithheld: boolean
  /** A file name guessed from the path, or the host when the path has none. */
  name: string
}

export interface Intake {
  links: ParsedLink[]
  duplicates: number
  /** Inputs that looked like addresses but cannot be downloaded, with the reason. */
  refused: { text: string; reason: string }[]
  /** Unique addresses past the limit, which are not taken. */
  overLimit: number
  tooLarge: boolean
}

function byteLength(s: string): number {
  let n = 0
  for (const ch of s) {
    const c = ch.codePointAt(0) ?? 0
    n += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4
  }
  return n
}

function decodeEntities(s: string): string {
  return s.replace(/&amp;/g, '&').replace(/&#38;/g, '&').replace(/&quot;/g, '"').replace(/&#39;/g, "'")
}

/** Candidate addresses in text: `href`/`src` attribute values of a pasted page, then bare scheme-prefixed words. */
export function candidates(text: string): string[] {
  const out: string[] = []
  const attr = /\b(?:href|src)\s*=\s*(?:"([^"]*)"|'([^']*)')/gi
  let m: RegExpExecArray | null
  let html = false
  while ((m = attr.exec(text))) {
    html = true
    out.push(decodeEntities((m[1] ?? m[2] ?? '').trim()))
  }
  const bare = /\b([a-z][a-z0-9+.-]{1,15}:\/\/[^\s"'<>]+)/gi
  const rest = html ? text.replace(attr, ' ') : text
  while ((m = bare.exec(rest))) out.push((m[1] ?? '').replace(/[),.;]+$/, ''))
  for (const line of text.split(/\r?\n/)) {
    const t = line.trim()
    if (/^(?:magnet:|ftp:\/\/|file:)/i.test(t) && !out.includes(t)) out.push(t)
  }
  return out
}

export function fileNameOf(u: URL): string {
  const seg = u.pathname.split('/').filter(Boolean).pop()
  if (!seg) return u.hostname
  try {
    return decodeURIComponent(seg)
  } catch {
    return seg
  }
}

/** Parses pasted text into links ready for review. */
export function intake(text: string): Intake {
  const tooLarge = byteLength(text) > MAX_INPUT_BYTES
  const src = tooLarge ? text.slice(0, MAX_INPUT_BYTES) : text
  const seen = new Set<string>()
  const links: ParsedLink[] = []
  const refused: Intake['refused'] = []
  let duplicates = 0
  let overLimit = 0
  for (const raw of candidates(src)) {
    if (/^magnet:/i.test(raw)) {
      refused.push({ text: raw.slice(0, 60), reason: 'Magnet links are not supported: Zeus has no torrent transport.' })
      continue
    }
    let u: URL
    try {
      u = new URL(raw)
    } catch {
      continue
    }
    if (u.protocol !== 'http:' && u.protocol !== 'https:') {
      refused.push({ text: raw.slice(0, 60), reason: `${u.protocol.replace(':', '')} addresses are refused; Zeus downloads http and https only.` })
      continue
    }
    if (u.username || u.password) {
      refused.push({ text: `${u.protocol}//${u.host}${u.pathname}`, reason: 'An address with a user name or password is refused. Site logins go through the Zeus extension.' })
      continue
    }
    u.hash = ''
    const key = u.toString()
    if (seen.has(key)) {
      duplicates++
      continue
    }
    seen.add(key)
    if (links.length >= MAX_URLS) {
      overLimit++
      continue
    }
    links.push({
      url: key,
      host: u.hostname.toLowerCase(),
      display: `${u.protocol}//${u.host}${u.pathname}`,
      queryWithheld: u.search.length > 1,
      name: fileNameOf(u),
    })
  }
  return { links, duplicates, refused, overLimit, tooLarge }
}

export interface Package<T extends { host: string }> {
  host: string
  items: T[]
}

/** Groups items into packages by host, largest package first, then by host name. */
export function packagesByHost<T extends { host: string }>(items: T[]): Package<T>[] {
  const map = new Map<string, T[]>()
  for (const it of items) map.set(it.host, [...(map.get(it.host) ?? []), it])
  return [...map.entries()].map(([host, list]) => ({ host, items: list })).sort((a, b) => b.items.length - a.items.length || a.host.localeCompare(b.host))
}

/** File names that would collide in one folder; the second keeps its place with " (1)" as zeusd's `rename` does. */
export function dedupeNames(names: string[]): string[] {
  const used = new Map<string, number>()
  return names.map((n) => {
    const key = n.toLowerCase()
    const count = used.get(key) ?? 0
    used.set(key, count + 1)
    if (count === 0) return n
    const dot = n.lastIndexOf('.')
    const base = dot > 0 ? n.slice(0, dot) : n
    const ext = dot > 0 ? n.slice(dot) : ''
    let candidate = `${base} (${count})${ext}`
    while (used.has(candidate.toLowerCase())) candidate = `${base} (${count + 1})${ext}`
    used.set(candidate.toLowerCase(), 1)
    return candidate
  })
}

/** A file name a person typed: refused when it names a folder, hides itself or uses characters Windows refuses. */
export function nameProblem(name: string): string | null {
  if (!name.trim()) return 'A name is needed.'
  if (/[\\/]/.test(name)) return 'A name cannot contain a folder separator.'
  if (/[<>:"|?*\u0000-\u001f]/.test(name)) return 'A name cannot contain < > : " | ? * or control characters.'
  if (/^\.+$/.test(name.trim())) return 'A name cannot be only dots.'
  if (/[ .]$/.test(name)) return 'A name cannot end with a space or a dot.'
  if (/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i.test(name)) return 'That name is reserved on Windows.'
  if (name.length > 240) return 'A name has at most 240 characters.'
  return null
}

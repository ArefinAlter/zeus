// The file search syntax: the conventions of Everything (voidtools) for a scoped index of the download roots. A space
// is AND, `|` is OR (binding tighter than AND), `!` negates a term, quotes keep spaces, `*` and `?` are wildcards that
// match the whole name. Functions narrow by property: `ext:`, `size:`, `dm:`, `path:`, `host:`, `file:`, `folder:`,
// `regex:`, `case:`, `ww:`. Pure functions with no dependencies, tested with node --test.

export interface SearchOptions {
  matchCase: boolean
  wholeWord: boolean
  /** Match the text against the full path instead of the name. */
  matchPath: boolean
  /** Treat the whole query as one regular expression. */
  regex: boolean
}

export const DEFAULT_OPTIONS: SearchOptions = { matchCase: false, wholeWord: false, matchPath: false, regex: false }

export interface Searchable {
  name: string
  dir: string
  isFolder: boolean
  size: number
  modified: number
  host?: string
}

type Cmp = '=' | '<' | '>' | '<=' | '>='

export type Term =
  | { kind: 'text'; neg: boolean; value: string; path: boolean; matchCase: boolean; wholeWord: boolean }
  | { kind: 'wild'; neg: boolean; re: RegExp; path: boolean }
  | { kind: 'regex'; neg: boolean; re: RegExp; path: boolean }
  | { kind: 'ext'; neg: boolean; exts: string[] }
  | { kind: 'size'; neg: boolean; min: number; max: number }
  | { kind: 'dm'; neg: boolean; from: number; to: number }
  | { kind: 'type'; neg: boolean; folder: boolean }
  | { kind: 'host'; neg: boolean; value: string }

/** AND of OR-groups. An empty list matches everything. */
export type Query = Term[][]

export type Parsed = { ok: true; query: Query } | { ok: false; error: string }

const UNITS: Record<string, number> = { b: 1, kb: 1024, k: 1024, mb: 1024 ** 2, m: 1024 ** 2, gb: 1024 ** 3, g: 1024 ** 3, tb: 1024 ** 4 }

/** Everything's size constants. */
export const SIZE_WORDS: Record<string, [number, number]> = {
  empty: [0, 0],
  tiny: [0, 10 * 1024],
  small: [10 * 1024, 100 * 1024],
  medium: [100 * 1024, 1024 ** 2],
  large: [1024 ** 2, 16 * 1024 ** 2],
  huge: [16 * 1024 ** 2, 128 * 1024 ** 2],
  gigantic: [128 * 1024 ** 2, Number.MAX_SAFE_INTEGER],
}

/** Parses `10mb`, `1.5 GB`, `512` (bytes) into bytes, or null. */
export function parseSize(s: string): number | null {
  const m = /^(\d+(?:\.\d+)?)\s*([a-z]*)$/i.exec(s.trim())
  if (!m) return null
  const unit = (m[2] ?? '').toLowerCase()
  const mult = unit === '' ? 1 : UNITS[unit]
  if (mult === undefined) return null
  return Math.round(Number(m[1]) * mult)
}

function splitCmp(v: string): [Cmp, string] {
  const m = /^(<=|>=|<|>|=)?(.*)$/.exec(v) as RegExpExecArray
  return [(m[1] as Cmp | undefined) ?? '=', m[2] ?? '']
}

function sizeRange(v: string): [number, number] | null {
  const word = SIZE_WORDS[v.toLowerCase()]
  if (word) return word
  if (v.includes('..')) {
    const [a, b] = v.split('..')
    const lo = parseSize(a ?? '')
    const hi = parseSize(b ?? '')
    if (lo === null || hi === null) return null
    return [Math.min(lo, hi), Math.max(lo, hi)]
  }
  const [cmp, rest] = splitCmp(v)
  const n = parseSize(rest)
  if (n === null) return null
  switch (cmp) {
    case '<':
      return [0, n - 1]
    case '<=':
      return [0, n]
    case '>':
      return [n + 1, Number.MAX_SAFE_INTEGER]
    case '>=':
      return [n, Number.MAX_SAFE_INTEGER]
    default:
      return [n, n]
  }
}

const DAY = 86_400_000

function startOfDay(t: number): number {
  const d = new Date(t)
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
}

/** A date or a named period as a half-open range [from, to) of epoch milliseconds, in local time. */
export function dateRange(v: string, now: number): [number, number] | null {
  const s = v.toLowerCase()
  const today = startOfDay(now)
  const d = new Date(now)
  const weekStart = today - ((d.getDay() + 6) % 7) * DAY // Monday
  switch (s) {
    case 'today':
      return [today, today + DAY]
    case 'yesterday':
      return [today - DAY, today]
    case 'thisweek':
      return [weekStart, weekStart + 7 * DAY]
    case 'lastweek':
      return [weekStart - 7 * DAY, weekStart]
    case 'thismonth':
      return [new Date(d.getFullYear(), d.getMonth(), 1).getTime(), new Date(d.getFullYear(), d.getMonth() + 1, 1).getTime()]
    case 'lastmonth':
      return [new Date(d.getFullYear(), d.getMonth() - 1, 1).getTime(), new Date(d.getFullYear(), d.getMonth(), 1).getTime()]
    case 'thisyear':
      return [new Date(d.getFullYear(), 0, 1).getTime(), new Date(d.getFullYear() + 1, 0, 1).getTime()]
  }
  const last = /^last(\d{1,3})(days?|weeks?|months?)$/.exec(s)
  if (last) {
    const n = Number(last[1])
    const unit = last[2] ?? 'days'
    if (unit.startsWith('day')) return [today - (n - 1) * DAY, today + DAY]
    if (unit.startsWith('week')) return [today - (7 * n - 1) * DAY, today + DAY]
    return [new Date(d.getFullYear(), d.getMonth() - n, d.getDate()).getTime(), today + DAY]
  }
  const ymd = /^(\d{4})(?:-(\d{1,2})(?:-(\d{1,2}))?)?$/.exec(s)
  if (ymd) {
    const y = Number(ymd[1])
    if (ymd[3]) {
      const from = new Date(y, Number(ymd[2]) - 1, Number(ymd[3])).getTime()
      return [from, from + DAY]
    }
    if (ymd[2]) return [new Date(y, Number(ymd[2]) - 1, 1).getTime(), new Date(y, Number(ymd[2]), 1).getTime()]
    return [new Date(y, 0, 1).getTime(), new Date(y + 1, 0, 1).getTime()]
  }
  return null
}

function dmRange(v: string, now: number): [number, number] | null {
  if (v.includes('..')) {
    const [a, b] = v.split('..')
    const lo = dateRange(a ?? '', now)
    const hi = dateRange(b ?? '', now)
    if (!lo || !hi) return null
    return [lo[0], hi[1]]
  }
  const [cmp, rest] = splitCmp(v)
  const r = dateRange(rest, now)
  if (!r) return null
  switch (cmp) {
    case '<':
      return [0, r[0]]
    case '<=':
      return [0, r[1]]
    case '>':
      return [r[1], Number.MAX_SAFE_INTEGER]
    case '>=':
      return [r[0], Number.MAX_SAFE_INTEGER]
    default:
      return r
  }
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** `*` and `?` as a regular expression over the whole name. */
export function wildcardRe(pattern: string, matchCase: boolean): RegExp {
  const body = pattern
    .split('')
    .map((c) => (c === '*' ? '.*' : c === '?' ? '.' : escapeRe(c)))
    .join('')
  return new RegExp(`^${body}$`, matchCase ? '' : 'i')
}

/** Splits on spaces outside quotes; `|` stays its own token. */
export function tokenize(q: string): string[] {
  const out: string[] = []
  let cur = ''
  let quoted = false
  for (const ch of q) {
    if (ch === '"') {
      quoted = !quoted
      cur += ch
    } else if (!quoted && /\s/.test(ch)) {
      if (cur) out.push(cur)
      cur = ''
    } else if (!quoted && ch === '|') {
      if (cur) out.push(cur)
      out.push('|')
      cur = ''
    } else cur += ch
  }
  if (quoted) cur += '"'
  if (cur) out.push(cur)
  return out
}

function unquote(s: string): string {
  return s.length >= 2 && s.startsWith('"') && s.endsWith('"') ? s.slice(1, -1) : s.replace(/"/g, '')
}

function parseTerm(raw: string, opts: SearchOptions, now: number): Term | string {
  let tok = raw
  let neg = false
  while (tok.startsWith('!')) {
    neg = !neg
    tok = tok.slice(1)
  }
  let matchCase = opts.matchCase
  let wholeWord = opts.wholeWord
  let path = opts.matchPath
  // Modifiers can stack: case:ww:path:foo
  for (;;) {
    const mod = /^(case|nocase|ww|wholeword|path|nopath):(.*)$/i.exec(tok)
    if (!mod) break
    const name = (mod[1] ?? '').toLowerCase()
    if (name === 'case') matchCase = true
    else if (name === 'nocase') matchCase = false
    else if (name === 'ww' || name === 'wholeword') wholeWord = true
    else if (name === 'path') path = true
    else path = false
    tok = mod[2] ?? ''
  }
  const fn = /^(ext|size|dm|datemodified|regex|host|file|folder):(.*)$/i.exec(tok)
  if (fn) {
    const name = (fn[1] ?? '').toLowerCase()
    const value = unquote(fn[2] ?? '')
    switch (name) {
      case 'ext': {
        const exts = value
          .split(/[;,]/)
          .map((e) => e.trim().replace(/^\./, '').toLowerCase())
          .filter(Boolean)
        if (!exts.length) return 'ext: needs one or more extensions, for example ext:pdf;docx'
        return { kind: 'ext', neg, exts }
      }
      case 'size': {
        const r = sizeRange(value)
        if (!r) return `size: does not understand "${value}". Try size:>10mb, size:1mb..5mb or size:large`
        return { kind: 'size', neg, min: r[0], max: r[1] }
      }
      case 'dm':
      case 'datemodified': {
        const r = dmRange(value, now)
        if (!r) return `dm: does not understand "${value}". Try dm:today, dm:lastweek, dm:2026-10 or dm:>2026-10-01`
        return { kind: 'dm', neg, from: r[0], to: r[1] }
      }
      case 'regex': {
        try {
          return { kind: 'regex', neg, re: new RegExp(value, matchCase ? '' : 'i'), path }
        } catch {
          return `regex: "${value}" is not a valid regular expression`
        }
      }
      case 'host':
        if (!value) return 'host: needs a host name, for example host:example.org'
        return { kind: 'host', neg, value: value.toLowerCase() }
      case 'file':
      case 'folder':
        if (value) return parseTerm(`${neg ? '!' : ''}${value}`, opts, now)
        return { kind: 'type', neg, folder: name === 'folder' }
    }
  }
  const value = unquote(tok)
  if (!value) return ''
  // A term with a path separator searches the path, as Everything does.
  const inPath = path || /[\\/]/.test(value)
  if (/[*?]/.test(value)) return { kind: 'wild', neg, re: wildcardRe(value, matchCase), path: inPath }
  return { kind: 'text', neg, value, path: inPath, matchCase, wholeWord }
}

/** Parses a query. With `opts.regex` the whole text is one regular expression. */
export function parseQuery(q: string, opts: SearchOptions = DEFAULT_OPTIONS, now = Date.now()): Parsed {
  const text = q.trim()
  if (!text) return { ok: true, query: [] }
  if (opts.regex) {
    try {
      return { ok: true, query: [[{ kind: 'regex', neg: false, re: new RegExp(text, opts.matchCase ? '' : 'i'), path: opts.matchPath }]] }
    } catch {
      return { ok: false, error: 'Not a valid regular expression.' }
    }
  }
  const tokens = tokenize(text)
  const groups: Term[][] = []
  let pendingOr = false
  for (const tok of tokens) {
    if (tok === '|') {
      pendingOr = true
      continue
    }
    const t = parseTerm(tok, opts, now)
    if (typeof t === 'string') {
      if (t === '') continue
      return { ok: false, error: t }
    }
    const last = groups[groups.length - 1]
    if (pendingOr && last) last.push(t)
    else groups.push([t])
    pendingOr = false
  }
  return { ok: true, query: groups }
}

function fullPath(e: Searchable): string {
  return e.dir ? `${e.dir}\\${e.name}` : e.name
}

function wordMatch(hay: string, needle: string): boolean {
  let i = hay.indexOf(needle)
  while (i !== -1) {
    const before = i === 0 ? '' : hay[i - 1] ?? ''
    const after = hay[i + needle.length] ?? ''
    if (!/[A-Za-z0-9]/.test(before) && !/[A-Za-z0-9]/.test(after)) return true
    i = hay.indexOf(needle, i + 1)
  }
  return false
}

function extOfName(name: string): string {
  const m = /\.([A-Za-z0-9]{1,10})$/.exec(name)
  return m ? (m[1] ?? '').toLowerCase() : ''
}

function termMatch(t: Term, e: Searchable): boolean {
  let hit: boolean
  switch (t.kind) {
    case 'text': {
      const hay0 = t.path ? fullPath(e) : e.name
      const hay = t.matchCase ? hay0 : hay0.toLowerCase()
      const needle = t.matchCase ? t.value : t.value.toLowerCase()
      hit = t.wholeWord ? wordMatch(hay, needle) : hay.includes(needle)
      break
    }
    case 'wild':
      hit = t.re.test(t.path ? fullPath(e) : e.name)
      break
    case 'regex':
      hit = t.re.test(t.path ? fullPath(e) : e.name)
      break
    case 'ext':
      hit = !e.isFolder && t.exts.includes(extOfName(e.name))
      break
    case 'size':
      hit = !e.isFolder && e.size >= t.min && e.size <= t.max
      break
    case 'dm':
      hit = e.modified >= t.from && e.modified < t.to
      break
    case 'type':
      hit = e.isFolder === t.folder
      break
    case 'host':
      hit = !!e.host && (e.host === t.value || e.host.endsWith('.' + t.value))
      break
  }
  return t.neg ? !hit : hit
}

export function matches(q: Query, e: Searchable): boolean {
  return q.every((group) => group.some((t) => termMatch(t, e)))
}

/** The result filters of the search bar, as Everything names them. */
export const FILTERS: { id: string; label: string; exts?: string[]; folder?: boolean }[] = [
  { id: 'everything', label: 'Everything' },
  { id: 'audio', label: 'Audio', exts: ['mp3', 'flac', 'wav', 'ogg', 'opus', 'm4a', 'aac', 'wma'] },
  { id: 'compressed', label: 'Compressed', exts: ['zip', 'rar', '7z', 'tar', 'gz', 'tgz', 'bz2', 'xz', 'zst', 'iso', 'cab'] },
  { id: 'document', label: 'Document', exts: ['pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'txt', 'md', 'epub', 'csv', 'json', 'rtf'] },
  { id: 'executable', label: 'Executable', exts: ['exe', 'msi', 'msix', 'bat', 'cmd', 'ps1', 'sh', 'jar', 'apk', 'appimage', 'dmg', 'pkg', 'deb', 'rpm'] },
  { id: 'folder', label: 'Folder', folder: true },
  { id: 'picture', label: 'Picture', exts: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'tif', 'tiff', 'heic', 'raw', 'bmp'] },
  { id: 'video', label: 'Video', exts: ['mp4', 'mkv', 'webm', 'mov', 'avi', 'm4v', 'wmv', 'ts'] },
]

export function filterMatch(filterId: string, e: Searchable): boolean {
  const f = FILTERS.find((x) => x.id === filterId)
  if (!f || f.id === 'everything') return true
  if (f.folder) return e.isFolder
  return !e.isFolder && (f.exts ?? []).includes(extOfName(e.name))
}

/** The syntax help, as the search bar's help panel lists it. */
export const SYNTAX: [string, string][] = [
  ['foo bar', 'both words (AND)'],
  ['foo | bar', 'either word (OR)'],
  ['!foo', 'not foo'],
  ['"two words"', 'an exact phrase with its space'],
  ['*.iso  report-??.pdf', 'wildcards over the whole name'],
  ['ext:pdf;epub', 'by extension'],
  ['size:>100mb  size:1mb..5mb  size:huge', 'by size (b, kb, mb, gb; empty, tiny, small, medium, large, huge, gigantic)'],
  ['dm:today  dm:lastweek  dm:last7days  dm:2026-10  dm:>2026-10-01', 'by date modified'],
  ['path:datasets  datasets\\', 'match the full path'],
  ['host:example.org', 'files downloaded from a host'],
  ['file:  folder:', 'only files, only folders'],
  ['regex:^report-\\d+', 'a regular expression'],
  ['case:Readme  ww:log', 'match case, whole word for one term'],
]

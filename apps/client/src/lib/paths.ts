// Paths as the engine writes them. The desktop app's engine reports absolute paths with the separator of its
// platform (a backslash on Windows, a slash elsewhere); the hosted pre-alpha and the sample data use backslashes
// for display, as the rest of the client always did. Everything that builds or splits a path goes through here.

let sep = '\\'

/** Set once the session says which platform the engine runs on. */
export function setSep(s: string | undefined): void {
  sep = s === '/' ? '/' : '\\'
}

/** The separator in use. */
export function SEP(): string {
  return sep
}

const split = (p: string): string[] => p.split(/[\\/]/)

/** Join parts with the platform separator, collapsing doubled separators at the joins. */
export function joinPath(...parts: string[]): string {
  const first = parts[0] ?? ''
  const rest = parts.slice(1).filter(Boolean)
  const head = first.replace(/[\\/]+$/, '') || (first.startsWith('/') ? '' : first)
  return [head, ...rest.map((p) => p.replace(/^[\\/]+|[\\/]+$/g, ''))].join(sep)
}

/** The folder part of a path (`C:\a\b.txt` gives `C:\a`; `/a/b.txt` gives `/a`). */
export function parentOf(path: string): string {
  const i = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'))
  if (i < 0) return ''
  if (i === 0) return path.slice(0, 1)
  return path.slice(0, i)
}

/** The last part of a path. */
export function baseOf(path: string): string {
  const t = path.replace(/[\\/]+$/, '')
  return split(t).pop() ?? t
}

const same = (a: string, b: string): boolean => (sep === '\\' ? a.toLowerCase() === b.toLowerCase() : a === b)

/** Whether `path` is `root` or lies inside it. */
export function isInside(root: string, path: string): boolean {
  const r = split(root.replace(/[\\/]+$/, ''))
  const p = split(path.replace(/[\\/]+$/, ''))
  if (p.length < r.length) return false
  return r.every((part, i) => same(part, p[i] ?? ''))
}

/** `path` below `root` as parts, or null when it is outside. */
export function below(root: string, path: string): string[] | null {
  if (!isInside(root, path)) return null
  return split(path.replace(/[\\/]+$/, '')).slice(split(root.replace(/[\\/]+$/, '')).length)
}

/** Whether a path is absolute on either platform. */
export function isAbsolute(path: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(path) || path.startsWith('\\\\') || path.startsWith('/')
}

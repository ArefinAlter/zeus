// Sizes, rates and times for people. Binary units (KiB, MiB, GiB) as zeusd reports them. Pure, tested with node --test.

export function fmtBytes(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return '–'
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return i === 0 ? `${Math.round(n)} B` : `${v.toFixed(v < 10 ? 2 : v < 100 ? 1 : 0)} ${units[i]}`
}

export function fmtRate(bps: number): string {
  if (!bps || bps < 1) return '–'
  return `${fmtBytes(bps)}/s`
}

/** "2 h 05 min", "4 min 10 s", "12 s"; null or infinite as an en dash. */
export function fmtEta(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds) || seconds < 0) return '–'
  const s = Math.round(seconds)
  if (s < 60) return `${s} s`
  if (s < 3600) return `${Math.floor(s / 60)} min ${String(s % 60).padStart(2, '0')} s`
  if (s < 86400) return `${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600) / 60)).padStart(2, '0')} min`
  return `${Math.floor(s / 86400)} d ${Math.floor((s % 86400) / 3600)} h`
}

/** Seconds left for `remaining` bytes at `rate` bytes per second; null when unknown or stalled. */
export function etaSeconds(remaining: number | null, rate: number): number | null {
  if (remaining === null || remaining < 0 || rate <= 0) return null
  return remaining / rate
}

/** Percent done with one decimal under 10 %, whole numbers above; null when the size is unknown. */
export function percent(done: number, total: number | null): number | null {
  if (total === null || total <= 0) return null
  return Math.max(0, Math.min(100, (done / total) * 100))
}

export function fmtPercent(p: number | null): string {
  if (p === null) return '–'
  if (p >= 100) return '100 %'
  return `${p < 10 ? p.toFixed(1) : p.toFixed(p < 99.95 ? 1 : 2)} %`
}

/** Parses "1.5 MiB", "200 KB", "2g" into bytes (binary units), or null. */
export function parseBytes(s: string): number | null {
  const m = /^\s*(\d+(?:\.\d+)?)\s*([kmgt]?)(i?b)?\s*$/i.exec(s)
  if (!m) return null
  const mult = { '': 1, k: 1024, m: 1024 ** 2, g: 1024 ** 3, t: 1024 ** 4 }[(m[2] ?? '').toLowerCase() as '' | 'k' | 'm' | 'g' | 't']
  return Math.round(Number(m[1]) * mult)
}

/** "today 14:05", "yesterday 09:12", "21 Oct 16:40", "21 Oct 2025": dates people scan in a list. */
export function fmtWhen(t: number | undefined, now = Date.now()): string {
  if (t === undefined) return '–'
  const d = new Date(t)
  const n = new Date(now)
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
  const sameDay = (a: Date, b: Date) => a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate()
  if (sameDay(d, n)) return `today ${hm}`
  const y = new Date(n.getFullYear(), n.getMonth(), n.getDate() - 1)
  if (sameDay(d, y)) return `yesterday ${hm}`
  const month = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'][d.getMonth()]
  return d.getFullYear() === n.getFullYear() ? `${d.getDate()} ${month} ${hm}` : `${d.getDate()} ${month} ${d.getFullYear()}`
}

/** "3 min ago", "in 2 min". */
export function fmtAgo(t: number | undefined, now = Date.now()): string {
  if (t === undefined) return '–'
  const s = Math.round((now - t) / 1000)
  const a = Math.abs(s)
  const v = a < 60 ? `${a} s` : a < 3600 ? `${Math.floor(a / 60)} min` : a < 86400 ? `${Math.floor(a / 3600)} h` : `${Math.floor(a / 86400)} d`
  return s >= 0 ? `${v} ago` : `in ${v}`
}

/** A full timestamp for facts: `2026-10-25 14:05:09 +06:00`. */
export function fmtStamp(t: number): string {
  const d = new Date(t)
  const p = (n: number) => String(n).padStart(2, '0')
  const off = -d.getTimezoneOffset()
  const sign = off >= 0 ? '+' : '-'
  const abs = Math.abs(off)
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())} ${sign}${p(Math.floor(abs / 60))}:${p(abs % 60)}`
}

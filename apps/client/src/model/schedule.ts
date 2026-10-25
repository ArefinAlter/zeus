// Queue schedules and the speed limiter: when a queue starts and stops, and what rate applies to a job. Times are
// local wall-clock `HH:MM`; days are 0 (Sunday) to 6. A stop time earlier than the start time means the window runs
// past midnight. Pure functions, tested with node --test.
//
// zeusd does not schedule queues yet (IMPLEMENTATION_STATUS: "daemon scheduling remains open"); this module is what
// the client shows and what the sample backend follows. It states its own rules so a later daemon can be tested
// against the same cases.

import type { QueueSchedule, SpeedLimiter, Weekday } from './types.ts'

const MIN = 60_000
const DAY = 86_400_000

/** Minutes after midnight for `HH:MM`, or null when the text is not a time. */
export function parseHm(s: string): number | null {
  const m = /^([01]?\d|2[0-3]):([0-5]\d)$/.exec(s.trim())
  if (!m) return null
  return Number(m[1]) * 60 + Number(m[2])
}

export function fmtHm(minutes: number): string {
  const m = ((minutes % 1440) + 1440) % 1440
  return `${String(Math.floor(m / 60)).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`
}

function at(dayStart: number, minutes: number): number {
  const d = new Date(dayStart)
  return new Date(d.getFullYear(), d.getMonth(), d.getDate(), Math.floor(minutes / 60), minutes % 60).getTime()
}

function dayStartOf(t: number): number {
  const d = new Date(t)
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
}

function addDays(dayStart: number, n: number): number {
  const d = new Date(dayStart)
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n).getTime()
}

/** `YYYY-MM-DD` of a local day. */
export function ymd(t: number): string {
  const d = new Date(t)
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

function parseYmd(s: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s)
  if (!m) return null
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime()
}

/**
 * The next start of a schedule strictly after `now` (or at `now`, with `inclusive`), or null when it has none: start
 * disabled, a bad time, a one-time date in the past, or a repeating schedule with no day selected.
 */
export function nextStart(s: QueueSchedule, now: number, inclusive = false): number | null {
  if (!s.startEnabled) return null
  const minutes = parseHm(s.startTime)
  if (minutes === null) return null
  const after = (t: number) => (inclusive ? t >= now : t > now)
  if (s.repeat === 'once') {
    const day = parseYmd(s.startDate)
    if (day === null) return null
    const t = at(day, minutes)
    return after(t) ? t : null
  }
  if (!s.days.length) return null
  const today = dayStartOf(now)
  for (let i = 0; i <= 7; i++) {
    const day = addDays(today, i)
    const wd = new Date(day).getDay() as Weekday
    if (!s.days.includes(wd)) continue
    const t = at(day, minutes)
    if (after(t)) return t
  }
  return null
}

/** The stop that follows a start at `startedAt`, or null when the schedule has no stop time. */
export function stopAfter(s: QueueSchedule, startedAt: number): number | null {
  if (!s.stopEnabled) return null
  const stop = parseHm(s.stopTime)
  if (stop === null) return null
  const day = dayStartOf(startedAt)
  let t = at(day, stop)
  if (t <= startedAt) t = at(addDays(day, 1), stop)
  return t
}

/** Whether a start fell due in the interval (prev, now]: the sample backend's tick asks this each second. */
export function startDue(s: QueueSchedule, prev: number, now: number): boolean {
  const n = nextStart(s, prev)
  return n !== null && n <= now
}

/** Whether a scheduled stop fell due in (prev, now], for a queue that started at `startedAt`. */
export function stopDue(s: QueueSchedule, startedAt: number | undefined, prev: number, now: number): boolean {
  if (startedAt === undefined) return false
  const t = stopAfter(s, startedAt)
  return t !== null && t > prev && t <= now
}

/** A plain description: "Daily at 23:00, stops at 06:00", "Mon, Wed, Fri at 02:30", "Once, 2026-10-26 at 01:00". */
export function describeSchedule(s: QueueSchedule): string {
  if (!s.startEnabled) return 'Starts by hand'
  const names = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
  let when: string
  if (s.repeat === 'once') when = `Once, ${s.startDate} at ${s.startTime}`
  else if (s.days.length === 7) when = `Daily at ${s.startTime}`
  else if (s.days.length === 5 && [1, 2, 3, 4, 5].every((d) => s.days.includes(d as Weekday))) when = `Weekdays at ${s.startTime}`
  else if (s.days.length === 2 && s.days.includes(0) && s.days.includes(6)) when = `Weekends at ${s.startTime}`
  else if (!s.days.length) when = 'No day selected'
  else when = `${[...s.days].sort((a, b) => ((a + 6) % 7) - ((b + 6) % 7)).map((d) => names[d]).join(', ')} at ${s.startTime}`
  return s.stopEnabled ? `${when}, stops at ${s.stopTime}` : when
}

/** Whether `now` lies inside a daily window [from, to) on one of `days`; a window past midnight belongs to its start day. */
export function inWindow(from: string, to: string, days: Weekday[], now: number): boolean {
  const f = parseHm(from)
  const t = parseHm(to)
  if (f === null || t === null || !days.length) return false
  const d = new Date(now)
  const minutes = d.getHours() * 60 + d.getMinutes()
  const today = d.getDay() as Weekday
  const yesterday = ((today + 6) % 7) as Weekday
  if (f === t) return days.includes(today)
  if (f < t) return days.includes(today) && minutes >= f && minutes < t
  // Past midnight: the evening part belongs to today, the morning part to yesterday's window.
  return (days.includes(today) && minutes >= f) || (days.includes(yesterday) && minutes < t)
}

/** Whether the global limit applies at `now`. */
export function limiterActive(l: SpeedLimiter, now: number): boolean {
  if (!l.enabled) return false
  if (!l.scheduleEnabled) return true
  return inWindow(l.from, l.to, l.days, now)
}

/**
 * The rate one job may use, in bytes per second: its own capacity, bounded by its queue's limit shared among the
 * queue's active jobs and by the global limit shared among all active jobs. `Infinity` means no limit applies.
 */
export function jobRateCap(opts: {
  capacity: number
  limiter: SpeedLimiter
  now: number
  activeJobs: number
  queueLimitKiBps: number | null
  queueActiveJobs: number
}): number {
  let cap = opts.capacity
  if (limiterActive(opts.limiter, opts.now) && opts.activeJobs > 0) cap = Math.min(cap, (opts.limiter.limitKiBps * 1024) / opts.activeJobs)
  if (opts.queueLimitKiBps !== null && opts.queueActiveJobs > 0) cap = Math.min(cap, (opts.queueLimitKiBps * 1024) / opts.queueActiveJobs)
  return cap
}

/** Minutes until a time, for "starts in 4 min" labels. */
export function minutesUntil(t: number, now: number): number {
  return Math.max(0, Math.ceil((t - now) / MIN))
}

export const ONE_DAY = DAY

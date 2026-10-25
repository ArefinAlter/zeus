// The columns of the downloads list: what each shows, how it sorts. The list keeps a download manager's density (one
// 32 px row per file, sortable and resizable columns) in this client's quieter style.

import type { ComponentChildren } from 'preact'

import { getState, queueName } from '../../backend/store'
import { categoryLabel } from '../../model/categories.ts'
import type { Job } from '../../model/types.ts'
import { etaSeconds, fmtBytes, fmtEta, fmtPercent, fmtRate, fmtWhen, percent } from '../../model/units.ts'
import { Progress, StateLabel } from '../../ui/components'
import { Icon, categoryIcon } from '../../ui/icons'

export const STATE_ORDER: Record<string, number> = {
  downloading: 0,
  probing: 1,
  verifying: 2,
  finalizing: 3,
  awaiting_approval: 4,
  awaiting_session: 5,
  retry_scheduled: 6,
  queued: 7,
  paused: 8,
  failed: 9,
  completed: 10,
  canceled: 11,
}

export function jobPercent(j: Job): number | null {
  if (j.state === 'completed') return 100
  if (j.state === 'verifying') return percent(j.hashed ?? 0, j.size)
  return percent(j.done, j.size)
}

export function jobEta(j: Job): number | null {
  if (j.state === 'downloading') return etaSeconds(j.size === null ? null : j.size - j.done, j.rate)
  if (j.state === 'verifying') return etaSeconds(j.size === null ? null : j.size - (j.hashed ?? 0), j.rate)
  return null
}

/** The status detail beside the state word: what happens next, in a few words. */
export function statusDetail(j: Job, now: number): string {
  switch (j.state) {
    case 'retry_scheduled':
      return j.retryAt ? `retry in ${Math.max(0, Math.ceil((j.retryAt - now) / 1000))} s` : 'retry scheduled'
    case 'awaiting_approval': {
      const ap = getState().approvals.find((a) => a.id === j.approvalId)
      return ap ? `waiting for you · ${ap.bindCode}` : 'waiting for you'
    }
    case 'awaiting_session':
      return 'needs a site login'
    case 'verifying':
      return 'hashing'
    case 'failed':
      return j.error?.code ?? 'failed'
    case 'completed':
      return j.quarantined ? 'quarantined' : j.matchedExpected ? 'matches published hash' : 'hashed'
    case 'paused':
      return `${fmtBytes(j.done)} saved`
    case 'queued':
      return j.queue ? queueName(j.queue) : 'waiting'
    default:
      return ''
  }
}

export interface Column {
  id: string
  label: string
  num?: boolean
  sort: (j: Job) => number | string
  cell: (j: Job, now: number) => ComponentChildren
}

export const COLUMNS: Column[] = [
  {
    id: 'name',
    label: 'Name',
    sort: (j) => j.name.toLowerCase(),
    cell: (j) => (
      <span class="namecell">
        <Icon name={categoryIcon(j.category)} />
        <span class="nm" title={j.name}>
          {j.name}
        </span>
        {j.quarantined ? (
          <span class="qflag" title={`Quarantined: ${(j.quarantineReasons ?? []).join(', ')}`}>
            <Icon name="shield" />
          </span>
        ) : null}
      </span>
    ),
  },
  { id: 'size', label: 'Size', num: true, sort: (j) => j.size ?? -1, cell: (j) => (j.size === null ? <span class="muted">unknown</span> : fmtBytes(j.size)) },
  {
    id: 'progress',
    label: 'Progress',
    sort: (j) => jobPercent(j) ?? -1,
    cell: (j) => {
      const p = jobPercent(j)
      const tone = j.state === 'failed' ? 'bad' : j.state === 'completed' ? 'ok' : j.state === 'paused' || j.state === 'queued' || j.state === 'canceled' ? 'muted' : j.state === 'awaiting_approval' ? 'warn' : ''
      return (
        <span class="progcell">
          <Progress value={j.state === 'downloading' && p === null ? null : (p ?? 0)} tone={tone} />
          <span class="pct mono">{j.state === 'canceled' ? '–' : fmtPercent(p)}</span>
        </span>
      )
    },
  },
  { id: 'speed', label: 'Speed', num: true, sort: (j) => (j.state === 'downloading' ? j.rate : -1), cell: (j) => (j.state === 'downloading' ? fmtRate(j.rate) : <span class="muted">–</span>) },
  { id: 'eta', label: 'Time left', num: true, sort: (j) => jobEta(j) ?? Number.MAX_SAFE_INTEGER, cell: (j) => fmtEta(jobEta(j)) },
  {
    id: 'status',
    label: 'Status',
    sort: (j) => STATE_ORDER[j.state] ?? 99,
    cell: (j, now) => (
      <span class="statuscell">
        <StateLabel state={j.state} />
        <span class="detail">{statusDetail(j, now)}</span>
      </span>
    ),
  },
  { id: 'added', label: 'Added', sort: (j) => j.added, cell: (j, now) => fmtWhen(j.added, now) },
  { id: 'category', label: 'Category', sort: (j) => j.category, cell: (j) => categoryLabel(j.category) },
  { id: 'queue', label: 'Queue', sort: (j) => (j.queue ? queueName(j.queue) : ''), cell: (j) => (j.queue ? queueName(j.queue) : <span class="muted">–</span>) },
  { id: 'host', label: 'Source host', sort: (j) => j.host, cell: (j) => <span class="mono small">{j.host}</span> },
]

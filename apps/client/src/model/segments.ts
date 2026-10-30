import type { Segment } from './types.ts'

/** Aggregate committed-byte progress. It does not invent per-worker ranges or speeds. */
export function computeTransferProgress(totalBytes: number | null, bytesDone: number, active: boolean): Segment[] {
  const total = totalBytes !== null && Number.isFinite(totalBytes) && totalBytes > 0 ? totalBytes : null
  const done = Math.max(0, Math.min(total ?? Number.MAX_SAFE_INTEGER, Number.isFinite(bytesDone) ? bytesDone : 0))
  if (!total && !done) return []
  return [{ start: 0, end: total ?? done, done, state: total !== null && done >= total ? 'done' : active ? 'active' : 'pending' }]
}

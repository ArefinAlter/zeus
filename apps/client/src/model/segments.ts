import type { Segment } from './types'

/**
 * Computes realistic dynamic chunk segments across parallel connections.
 * Matches multi-connection segmented downloads (e.g., 8-32 parallel Range streams).
 */
export function computeDynamicSegments(
  totalBytes: number | null,
  bytesDone: number,
  connections: number,
  isActive: boolean,
): Segment[] {
  if (!totalBytes || totalBytes <= 0) {
    if (bytesDone > 0) {
      return [{ start: 0, end: bytesDone, done: bytesDone, state: isActive ? 'active' : 'done' }]
    }
    return []
  }

  const numConn = Math.max(1, Math.min(connections || 8, 32))
  if (totalBytes < 128 * 1024 || numConn <= 1) {
    const isDone = bytesDone >= totalBytes
    return [
      {
        start: 0,
        end: totalBytes,
        done: Math.min(totalBytes, bytesDone),
        state: isDone ? 'done' : isActive ? 'active' : 'pending',
      },
    ]
  }

  const chunkSize = Math.floor(totalBytes / numConn)
  const segments: Segment[] = []
  const isComplete = bytesDone >= totalBytes
  const avgPerChunk = bytesDone / numConn

  for (let i = 0; i < numConn; i++) {
    const start = i * chunkSize
    const end = i === numConn - 1 ? totalBytes : (i + 1) * chunkSize
    const span = end - start

    let done = 0
    let state: Segment['state'] = 'pending'

    if (isComplete) {
      done = span
      state = 'done'
    } else if (bytesDone > 0) {
      // Deterministic slight stream variance simulating real-world network buffer arrivals
      const factor = 0.85 + ((i * 37) % 30) / 100
      const targetDone = Math.min(span, Math.round(avgPerChunk * factor))
      done = Math.max(0, Math.min(span, targetDone))
      state = done >= span ? 'done' : isActive && done > 0 ? 'active' : 'pending'
    }

    segments.push({ start, end, done, state })
  }

  return segments
}

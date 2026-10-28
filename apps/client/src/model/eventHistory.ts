// Display only: raw event payloads, remote text and addresses never become history labels.
const labels: Record<string, string> = {
  'job.created': 'Enqueued in zeusd',
  'job.probed': 'Source checked',
  'job.throttled': 'Transfer rate limited',
  'job.retry_scheduled': 'Retry scheduled',
  'session.refresh_needed': 'Session refresh requested',
  'session.leased': 'Session access received',
  'job.verified': 'File verified',
  'job.quarantined': 'File quarantined',
  'job.completed': 'Download completed',
  'job.failed': 'Download failed',
}
const states = new Set(['queued', 'awaiting_approval', 'probing', 'downloading', 'paused', 'retry_scheduled', 'awaiting_session', 'verifying', 'finalizing', 'completed', 'failed', 'canceled'])

export function historyLabel(kind: string, state: string | null): string | null {
  if (kind === 'job.state_changed') return state && states.has(state) ? `zeusd: ${state.replaceAll('_', ' ')}` : null
  return Object.hasOwn(labels, kind) ? labels[kind]! : null
}

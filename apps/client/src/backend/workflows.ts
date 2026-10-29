import { call } from './remote'

export type Finding = { code: string; message: string; severity: string; path?: string }
export type PlanReview = {
  plan_id: string; plan_hash?: string; browser_review?: string; plan?: Record<string, unknown>
  diagnostics: Finding[]; samples: { url_redacted: string; body: string }[]
}
export type DryRun = {
  complete: boolean; plan_hash: string; sampled_pages: number; name_collisions: number
  sample: { url_redacted: string; file_name: string; collides_with?: number }[]
  warnings: Finding[]
}
export type WorkflowRun = {
  run_id: string; plan_id: string | null; plan_hash: string; status: string; approval_id: string | null; browser_document_mode?: 'browser_origin'
  pages_fetched: number; files_enqueued: number; files_completed: number; files_failed: number; bytes_done: number
  elapsed_ms: number; possibly_incomplete: boolean; pause_reason: string | null; stop_reason: string | null
  last_error: { code: string; message: string } | null; drift: Finding[]; counts?: Record<string, number> | null
}
export type RunPage = { items: WorkflowRun[]; next_cursor: string | null }
export type SavedPlan = { plan_id: string; goal: string; urls: string; text: string; plan_hash: string }

// A mutation retains its key at the call site until success, so a lost response can be retried safely.
export const workflowApi = {
  plan: (params: unknown) => call<PlanReview>('POST', '/native/v1/workflows/plan', params),
  dry: (plan_id: string) => call<DryRun>('POST', '/native/v1/workflows/dry-run', { plan_id, sample_items: 20 }),
  run: (plan_id: string, plan_hash: string, idempotency_key: string) => call<{ run_id: string; status: string; approval_id?: string }>('POST', '/native/v1/workflows/run', { plan_id, plan_hash, idempotency_key }),
  list: (cursor: string | undefined) => call<RunPage>('POST', '/native/v1/workflows/list', { limit: 50, ...(cursor ? { cursor } : {}) }),
  get: (run_id: string) => call<WorkflowRun>('POST', '/native/v1/workflows/get', { run_id }),
  control: (run_id: string, action: 'pause' | 'resume' | 'cancel', idempotency_key: string) => call<{ run: WorkflowRun }>('POST', '/native/v1/workflows/control', { run_id, action, idempotency_key }),
  manifest: (run_id: string, format: 'jsonl' | 'csv', idempotency_key: string) => call<{ path: string; records: number; content?: string; sha256?: string }>('POST', '/native/v1/workflows/manifest', { run_id, format, idempotency_key }),
  saved: async (): Promise<SavedPlan[]> => {
    const state = await call<{ slices: Record<string, { value: unknown }> }>('GET', '/svc/v1/state')
    const value = state.slices.workflow_drafts?.value
    if (!Array.isArray(value)) return []
    return value.filter((p): p is SavedPlan => !!p && ['plan_id', 'goal', 'urls', 'text', 'plan_hash'].every(key => typeof p[key] === 'string')).slice(0, 30)
  },
  save: (plans: SavedPlan[]) => call('PUT', '/svc/v1/state/workflow_drafts', plans),
  review: (approval: string) => call('POST', '/native/v1/cloud/review-terminal', { action: 'approve', id: approval }),
}

export function workflowTemplate(urls: string[], goal: string, dest: string): Record<string, unknown> {
  const domains = [...new Set(urls.map(raw => {
    const u = new URL(raw)
    if (!['http:', 'https:'].includes(u.protocol) || u.username || u.password) throw new Error('Use public HTTP or HTTPS pages without embedded credentials.')
    return u.hostname
  }))]
  if (!urls.length || urls.length > 3 || !dest.trim()) throw new Error('Enter one to three page URLs and a destination folder.')
  return {
    plan_version: 0, name: goal.trim().slice(0, 80) || 'Linked files', goal: goal.trim(), allowed_domains: domains,
    sources: [{ type: 'start_urls', urls }],
    extract: { items: { kind: 'css', query: 'a[download]' }, fields: { url: { extract: { kind: 'attr', name: 'href' } } } },
    fetch: { min_interval_ms: 1000, max_concurrency_per_host: 1, respect_robots: true, max_connections_per_file: 4 },
    budgets: { max_pages: 5, max_files: 20, max_bytes: 104857600, max_duration_ms: 600000, max_depth: 1 },
    sinks: { dest_dir: dest.trim() },
  }
}

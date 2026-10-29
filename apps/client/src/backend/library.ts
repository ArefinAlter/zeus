import { call } from './remote'

export type Artifact = {
  id: string; name: string | null; state: 'active' | 'deleted'; size_bytes: number; sha256: string; blake3: string
  quarantined: boolean; retention: string; retain_until: string | null; object: { state: string }; [key: string]: unknown
}
export type ArtifactPage = { items: Artifact[]; next_cursor?: string | null }
export type Lifecycle = { plan: { artifact_id: string; requires_approval: boolean; expires_at: string; [key: string]: unknown }; plan_hash: string }
export type LifecycleReceipt = { applied: boolean; approval_id?: string; artifact?: Artifact; path?: string; replayed: boolean }
export type Lineage = { nodes: { artifact_id: string | null; blake3: string; size_bytes: number; depth: number; links: { node: number; role: string }[] }[]; truncated: boolean }
export type OperationStatus = { operation_id: string; state: string; reason: string | null; steps: FileStep[]; completed: number; skipped: number; pending: number; approval_id: string; undone_by: string | null }
export type FileStep = { op: string; from: string; to?: string; destructive: boolean }
export type Organization = { steps: FileStep[]; applied: boolean; approval_id?: string; operation_id?: string; replayed: boolean }
export type MaintenanceOperation =
  | { kind: 'export'; artifact_ids: string[]; path: string; include_objects: boolean }
  | { kind: 'import'; path: string }
  | { kind: 'collect'; quota_pressure: boolean; limit: number }
  | { kind: 'repair'; artifact_id: string; path: string }
  | { kind: 'scrub'; artifact_ids: string[] }
export type MaintenancePreview = { plan: { operation: MaintenanceOperation; requires_approval: boolean; expires_at: string; [key: string]: unknown }; plan_hash: string }
export type MaintenanceRequest = MaintenancePreview & { idempotency_key: string }
export type MaintenanceReceipt = {
  status: string; applied: boolean; replayed: boolean; error_code: string | null; approval_id: string | null
  artifact_ids: string[]; size_bytes: number; absent_objects: number | null; remaining_objects: number | null
  reconciliation_required: boolean; verified_objects: string[]; damaged_objects: string[]
}
export type ArtifactEvents = { events: Record<string, unknown>[]; next_seq: number; has_more: boolean; history_truncated: boolean; earliest_seq: number; head: { chain_id: string }; store_format_version: number }
const artifact = <T>(op: string, params: unknown) => call<T>('POST', `/native/v1/artifacts/${op}`, params)
export const libraryApi = {
  list: (cursor: string | undefined, state: string) => artifact<ArtifactPage>('list', { limit: 50, ...(cursor ? { cursor } : {}), ...(state ? { state } : {}) }),
  get: (artifact_id: string) => artifact<{ artifact: Artifact }>('get', { artifact_id }),
  lineage: (artifact_id: string, direction: 'ancestors' | 'descendants') => artifact<Lineage>('lineage', { artifact_id, direction, max_depth: 8, max_nodes: 100 }),
  ingest: (path: string, idempotency_key: string) => artifact<{ artifact: Artifact; created: boolean }>('ingest', { path, idempotency_key }),
  preview: (artifact_id: string, operation: unknown) => artifact<Lifecycle>('preview', { artifact_id, operation }),
  apply: (preview: Lifecycle, idempotency_key: string) => artifact<LifecycleReceipt>('apply', { ...preview, idempotency_key }),
  maintenancePreview: (operation: MaintenanceOperation) => artifact<MaintenancePreview>('maintenance-preview', { operation }),
  maintenanceApply: (request: MaintenanceRequest) => artifact<MaintenanceReceipt>('maintenance-apply', request),
  maintenanceStatus: (request: MaintenanceRequest) => artifact<MaintenanceReceipt>('maintenance-status', request),
  events: (after_seq = 0, chain_id?: string) => artifact<ArtifactEvents>('events', { after_seq, limit: 20, ...(chain_id ? { chain_id } : {}) }),
  organize: (op: 'plan' | 'request', dir: string, rules: unknown, idempotency_key: string) => call<Organization>('POST', `/native/v1/organize/${op}`, { dir, rules, idempotency_key }),
  operation: (operation_id: string) => call<OperationStatus>('POST', '/native/v1/organize/get', { operation_id }),
  undo: (operation_id: string, idempotency_key: string) => call<Organization>('POST', '/native/v1/organize/undo', { operation_id, idempotency_key }),
  review: (id: string) => call('POST', '/native/v1/cloud/review-terminal', { action: 'approve', id }),
}

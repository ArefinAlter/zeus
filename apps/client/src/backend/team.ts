import { call } from './remote'
export type TeamFeatures = { downloads: boolean; cloud_delivery: boolean; workflows: boolean; metadata_sync: boolean; provenance: boolean }
export type TeamPolicy = { sequence: number; expires_at: string; features: TeamFeatures }
export type TeamStatus = { configured: boolean; trusted: boolean; connected: boolean; origin: string | null; tenant_id: string | null; policy: TeamPolicy | null; policy_held: boolean }
export type TeamKind = 'tags' | 'queue_presets' | 'workspace_labels'
export type TeamRecord = { kind: TeamKind; record_id: string; revision: number; sequence: number; tombstone: boolean; value: Record<string, unknown> | null }
export type TeamEnrollment = { user_code: string; verification_uri: string; expires_in_ms: number; poll_interval_ms: number }
const request = <T>(operation: string, params: unknown = {}) => call<T>('POST', `/native/v1/team/${operation}`, params)
export const teamApi = {
  status: () => request<TeamStatus>('status'),
  connect: (alias: string) => request<TeamEnrollment>('connect', { alias }),
  open: () => request('open'),
  poll: () => request<{ state: 'waiting' | 'connected'; poll_after_ms?: number }>('poll'),
  cancel: () => request('cancel'),
  refresh: () => request('refresh'),
  rotate: () => request('rotate'),
  disconnect: () => request('disconnect'),
  changes: (after: number) => request<{ records: TeamRecord[]; more: boolean; snapshot_sequence: number }>('changes', { after }),
  put: (kind: TeamKind, record_id: string, revision: number, value: unknown, idempotency_key: string) => request('put', { kind, record_id, revision, value, idempotency_key }),
  delete: (r: TeamRecord, idempotency_key: string) => request('delete', { kind: r.kind, record_id: r.record_id, revision: r.revision, value: null, idempotency_key }),
  provenance: (sha256: string, size_bytes: number, source_kind: string, idempotency_key: string) => request<{ verified: true; assertion: unknown; envelope: unknown }>('provenance', { artifact: { sha256, size_bytes, source_kind }, idempotency_key }),
}

import { call } from './remote'
export type SourceBudget = { max_received_bytes: number; timeout_ms: number }
export type SourceScope =
  | { provider: 's3'; region: string; bucket: string; key_prefix: string }
  | { provider: 'hugging_face'; repository_type: 'model' | 'dataset' | 'space'; repository: string }
  | { provider: 'torrent'; max_peers: number; max_download_bytes_per_second: number; allowed_peer_ips: string[] }
export type SourceAccount = { account_id: string; untrusted_label: string; revision: number; config: SourceScope; budget_ceiling: SourceBudget; revoked: boolean }
export type SourceMetadata = { bytes: number; strong_etag: string | null; sha256: string | null; immutable_revision: string; integrity_kind: string }
export type SourcePlan = { plan_id: string; account_id: string; account_revision: number; untrusted_label: string; metadata: SourceMetadata; budget: SourceBudget; expires_at: string; notices: string[]; torrent_files?: { index: number; untrusted_path: string[]; bytes: number }[] }
export type SourceOperation = {
  operation_id: string; account_id: string; state: string; untrusted_label: string; received_bytes: number; payload_uploaded_bytes: number
  metadata: SourceMetadata; budget: SourceBudget; output_sha256: string | null; error_code: string | null
  files: { index: number; filename: string; bytes: number; sha256: string }[]
}
const request = <T>(operation: string, params: unknown = {}) => call<T>('POST', `/native/v1/sources/${operation}`, params)
export const sourcesApi = {
  accounts: () => request<{ accounts: SourceAccount[] }>('accounts'),
  operations: () => request<{ operations: SourceOperation[] }>('list'),
  inspect: (params: unknown) => request<SourcePlan>('inspect', params),
  submit: (params: unknown) => request<SourceOperation>('submit', params),
  control: (params: unknown) => request<SourceOperation>('control', params),
  setup: (action: 'configure' | 'revoke', id = '') => request('review-terminal', { action, id }),
}

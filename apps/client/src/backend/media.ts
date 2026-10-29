// Native media facts never contain signed URLs, cookie material or arbitrary RPC.
import { call } from './remote'

export interface MediaFormat {
  format_id: string
  label: string
  container: string
  mime_type: string | null
  bandwidth: number | null
  width: number | null
  height: number | null
  segment_count: number | null
  duration_ms: number | null
  notices: string[]
}

export interface MediaInspection {
  inspection_id: string
  formats: MediaFormat[]
  origins: string[]
  expires_in_ms?: number
  needs_selection?: true
}

export interface MediaPlan {
  plan_id: string
  format: MediaFormat
  fingerprint: string
  filename: string
  origins: string[]
  roots: string[]
  default_max_bytes: number
  expires_in_ms: number
}

export interface MediaTask {
  task_id: string
  kind: 'acquisition' | 'mux'
  recovery_available: boolean
  fingerprint: string
  label: string
  state: 'downloading' | 'assembling' | 'converting' | 'interrupted' | 'canceled' | 'failed' | 'complete'
  filename: string
  root_index: number
  segment_count: number
  completed_segments: number
  verified_bytes: number
  received_bytes: number
  output_bytes: number | null
  sha256: string | null
  error_code: string | null
  error_message: string | null
  source_task_ids: string[]
  helper_sha256: string | null
}

export interface MediaHelper {
  configured: boolean
  executable?: string
  sha256?: string
  limits?: string
  message?: string
}

export interface MediaMuxPlan {
  plan_id: string
  fingerprint: string
  filename: string
  container: 'mp4' | 'mkv'
  inputs: { task_id: string; filename: string; bytes: number; sha256: string }[]
  input_bytes: number
  roots: string[]
  helper: MediaHelper
  notices: string[]
  default_max_bytes: number
  default_timeout_ms: number
  expires_in_ms: number
}

export const media = {
  inspect: (url: string, additional_origins: string[]) => call<MediaInspection>('POST', '/native/v1/media/inspect', { url, additional_origins }),
  select: (inspection_id: string, format_id: string) => call<MediaPlan | MediaInspection>('POST', '/native/v1/media/select', { inspection_id, format_id }),
  tasks: () => call<{ items: MediaTask[]; unreadable_checkpoints: number }>('GET', '/native/v1/media/tasks'),
  start: (body: { plan_id: string; root_index: number; filename: string; max_bytes: number; connections: number; idempotency_key: string; resume_task_id?: string }) => call<MediaTask>('POST', '/native/v1/media/start', body),
  cancel: (task_id: string) => call<MediaTask>('POST', '/native/v1/media/cancel', { task_id }),
  recover: (task_id: string) => call<MediaTask>('POST', '/native/v1/media/recover', { task_id }),
  archive: (task_id: string) => call<{ ok: true; output_preserved: true }>('POST', '/native/v1/media/archive', { task_id }),
  helper: () => call<MediaHelper>('GET', '/native/v1/media/helper'),
  muxPreview: (task_ids: string[], container: 'mp4' | 'mkv') => call<MediaMuxPlan>('POST', '/native/v1/media/mux-preview', { task_ids, container }),
  muxStart: (body: { plan_id: string; root_index: number; filename: string; max_bytes: number; timeout_ms: number; idempotency_key: string }) => call<MediaTask>('POST', '/native/v1/media/mux-start', body),
}

// The client's data model. Names and states follow the Zeus specifications (01 domain model, 07 approvals, 06 MCP
// tools) so the screens read the same words a real zeusd reports. This file holds types only.

/** Job states, exactly as spec 01 section 6 names them. */
export type JobState =
  | 'queued'
  | 'probing'
  | 'awaiting_approval'
  | 'awaiting_session'
  | 'downloading'
  | 'paused'
  | 'retry_scheduled'
  | 'verifying'
  | 'finalizing'
  | 'completed'
  | 'failed'
  | 'canceled'

export type CategoryId = 'compressed' | 'documents' | 'music' | 'programs' | 'video' | 'pictures' | 'general'

export interface Segment {
  start: number
  end: number
  /** Bytes of this interval already committed. */
  done: number
  state: 'pending' | 'active' | 'done' | 'failed'
}

export type SourceKind = 'cli' | 'agent' | 'browser_handoff' | 'workflow' | 'app'

export interface JobSource {
  kind: SourceKind
  /** Who asked, as a person reads it: "Claude Code (MCP)", "You, in Zeus", "Edge, browser handoff". */
  label: string
}

export interface JobError {
  /** The stable code, for example `http.url_expired`. */
  code: string
  /** What happened, calmly. */
  message: string
  /** The next step. */
  next: string
}

export interface JobEvent {
  t: number
  text: string
}

export interface Job {
  /** `job_` id. */
  id: string
  name: string
  /** The address as shown: scheme, host and path. A query string is never shown (redact_url). */
  url: string
  host: string
  /** True when the real address carries a query string that is withheld from display. */
  queryWithheld: boolean
  size: number | null
  done: number
  /** Bytes per second while downloading or verifying. */
  rate: number
  state: JobState
  category: CategoryId
  queue: string | null
  added: number
  finished?: number
  dir: string
  source: JobSource
  segments: Segment[]
  connections: number
  resumable: 'verified' | 'unsupported' | 'unknown'
  priority: 'low' | 'normal' | 'high'
  sha256?: string
  blake3?: string
  expectedSha256?: string
  /** Completed and verified are separate facts: a completed file was hashed; this says it matched an expected hash. */
  matchedExpected?: boolean
  quarantined?: boolean
  quarantineReasons?: string[]
  error?: JobError
  retryAt?: number
  approvalId?: string
  grabberProject?: string
  sessionOrigin?: string
  /** Native: the finished file is no longer in the download folders (removed, moved or excluded from the index). */
  missing?: boolean
  description?: string
  events: JobEvent[]
  /** Bytes hashed so far while verifying. */
  hashed?: number
  /** When a probe that is in flight answers (sample backend). */
  probeUntil?: number
  /** Scripted behaviour of the sample backend; never shown. */
  script?: { failAt?: number; retryAt?: number; maxRate?: number }
}

export type Weekday = 0 | 1 | 2 | 3 | 4 | 5 | 6

export type OnFinish = 'nothing' | 'notify' | 'stop_limiter' | 'close_app' | 'sleep'

export interface QueueSchedule {
  /** Start at a time. */
  startEnabled: boolean
  /** `HH:MM`, local time. */
  startTime: string
  /** `once` runs on `startDate`; `days` repeats on the listed weekdays. */
  repeat: 'once' | 'days'
  startDate: string
  days: Weekday[]
  stopEnabled: boolean
  stopTime: string
}

export interface Queue {
  id: string
  name: string
  /** A synchronization queue checks its finished files for newer versions on a period instead of downloading once. */
  kind: 'download' | 'sync'
  builtin: boolean
  running: boolean
  maxConcurrent: number
  retries: number
  schedule: QueueSchedule
  onFinish: OnFinish
  /** KiB/s for the whole queue; null means no queue limit. */
  limitKiBps: number | null
  /** For a synchronization queue: check every this many hours. */
  syncEveryHours: number
  syncFiles?: { job_id: string; pending_job_id: string | null; last_checked_ms: number | null; next_check_ms: number; status: string }[]
  order: string[]
  lastStarted?: number
}

export interface SpeedLimiter {
  enabled: boolean
  /** KiB/s for everything Zeus downloads. */
  limitKiBps: number
  scheduleEnabled: boolean
  /** The limiter applies only inside this window when the schedule is on. `HH:MM` local. */
  from: string
  to: string
  days: Weekday[]
}

export type ApprovalKind = 'handoff' | 'session_grant' | 'retry_escalation' | 'policy_exception' | 'workflow_plan' | 'destructive_op' | 'cloud_delivery'
export type ApprovalState = 'pending' | 'approved' | 'denied' | 'expired' | 'canceled'
export type Scope = 'once' | 'site' | 'workflow' | 'never'

export interface ApprovalFacts {
  t: 'zeus.approval.facts.v1'
  approval_id: string
  revision: number
  kind: ApprovalKind
  website: string
  requester: { kind: 'agent' | 'human' | 'service'; label: string }
  for: string
  files: { name: string; size: number | null }[]
  destination: string
  allowed_scopes: Scope[]
  confirmation: 'gesture' | 'typed_code'
  issued_at: string
  expires_at: string
}

export interface Approval {
  id: string
  state: ApprovalState
  facts: ApprovalFacts
  /** Hex digest of the canonical facts. */
  requestHash: string
  /** 30-bit bind code, `XXX-XXX`. */
  bindCode: string
  /** The requester's own words; never a fact. */
  agentSays?: string
  jobIds: string[]
  decided?: { at: number; decision: 'approve' | 'deny'; scope?: Scope; via: string }
  /** Live backend: who holds the request. zeusd's are listed here and decided only on zeusd's trust surfaces. */
  heldBy?: 'zeusd' | 'service'
}

export type AgentAccess = 'read' | 'download' | 'organize'

export interface ToolInfo {
  /** The MCP tool name, exactly as zeus-mcp lists it. */
  name: string
  title: string
  access: AgentAccess
  /** What it does, in one line. */
  summary: string
  /** Mirrors the MCP annotations. */
  readOnly: boolean
  openWorld: boolean
  /** Actions of a multi-action tool that need a higher access than the tool's own. */
  actions?: { name: string; access: AgentAccess }[]
}

export interface FileEntry {
  id: number
  name: string
  /** Folder path with a trailing separator removed. */
  dir: string
  isFolder: boolean
  size: number
  /** Epoch milliseconds. */
  modified: number
  sha256?: string
  jobId?: string
  quarantined?: boolean
  host?: string
}

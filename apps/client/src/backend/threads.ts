// Agent threads: what a person asked, what the agent said, the tools it called (with refusals) and the cards that came
// out of them. A card carries anything with consequences; its button is pressed by the person.

import type { AgentAccess } from '../model/types.ts'

export interface BatchItem {
  name: string
  size: number | null
  host: string
  url: string
  expected?: string
}

export type Card =
  | { type: 'batch'; title: string; items: BatchItem[]; dest: string; done?: 'grabber' | 'started'; startedIds?: string[] }
  | { type: 'approvals'; ids: string[] }
  | { type: 'job'; id: string }
  | { type: 'retry'; jobId: string; done?: boolean }
  | { type: 'table'; title: string; columns: string[]; rows: string[][]; source?: string }
  | { type: 'organize'; moves: { fileId: number; jobId?: string; name: string; fromDir: string; toDir: string }[]; approvalId?: string }
  | { type: 'cloud'; destination: string; files: { name: string; size: number | null }[]; mode: string; approvalId?: string }
  | { type: 'probe'; url: string; status: number; size: number | null; contentType: string; resumable: boolean; login: boolean }

export type AgentItem =
  | { kind: 'user'; id: string; text: string }
  | { kind: 'assistant'; id: string; text: string; streaming?: boolean }
  | { kind: 'tool'; id: string; tool: string; action?: string; input: unknown; output?: unknown; state: 'running' | 'ok' | 'denied' | 'error'; summary: string; ms?: number }
  | { kind: 'card'; id: string; card: Card }
  | { kind: 'notice'; id: string; text: string; tone: 'info' | 'warn' }

export interface Thread {
  id: string
  title: string
  access: AgentAccess
  state: 'idle' | 'running'
  created: number
  updated: number
  items: AgentItem[]
  /** The skill in use, when a request matched one. */
  skill?: string
}

let seq = 1
export function itemId(): string {
  return `it${(seq++).toString(36)}`
}

/** A thread's title: the first request, cut at a word. */
export function titleOf(text: string): string {
  const t = text.replace(/\s+/g, ' ').trim()
  if (!t) return 'New thread'
  if (t.length <= 52) return t
  const cut = t.slice(0, 52)
  const at = cut.lastIndexOf(' ')
  return (at > 28 ? cut.slice(0, at) : cut).replace(/[.,;:!?]+$/, '') + '…'
}

export function sampleThreads(now: number): Thread[] {
  const h = 3_600_000
  return [
    {
      id: 'th_verify',
      title: 'Verify the sensor readings download',
      access: 'read',
      state: 'idle',
      created: now - 5 * 24 * h,
      updated: now - 5 * 24 * h + 60_000,
      skill: 'verify-checksums',
      items: [
        { kind: 'user', id: itemId(), text: 'Verify the sensor readings download against the checksum on the release page' },
        { kind: 'notice', id: itemId(), text: 'Using the skill Verify checksums: it may call zeus_files_list, zeus_verify and zeus_status.', tone: 'info' },
        { kind: 'tool', id: itemId(), tool: 'zeus_files_list', input: { limit: 50 }, output: { files: 1, untrusted_path: 'D:\\Downloads\\Zeus\\Documents\\sensor-readings-2026-q3.json' }, state: 'ok', summary: '1 matching file', ms: 120 },
        { kind: 'tool', id: itemId(), tool: 'zeus_verify', input: { job_id: 'job_…', expected_sha256: '9f2c…e41a' }, output: { matches_recorded: true, matches_expected: true }, state: 'ok', summary: 'matches the recorded and the expected SHA-256', ms: 1840 },
        { kind: 'assistant', id: itemId(), text: 'The file **sensor-readings-2026-q3.json** (64 MiB) re-hashed to the SHA-256 recorded when it finished, and it matches the checksum you gave me from the release page.\n\nI checked one file. I did not open it or read its contents.' },
      ],
    },
    {
      id: 'th_lectures',
      title: 'Batch the lecture videos from the course page',
      access: 'organize',
      state: 'idle',
      created: now - 26 * h,
      updated: now - 26 * h + 90_000,
      skill: 'batch-from-page',
      items: [
        { kind: 'user', id: itemId(), text: 'Get the lecture videos from https://media.example.edu/courses/dsp/ into the off-peak queue' },
        { kind: 'notice', id: itemId(), text: 'Using the skill Batch from a page: it may call zeus_workflow, zeus_probe and zeus_status.', tone: 'info' },
        { kind: 'tool', id: itemId(), tool: 'zeus_workflow', action: 'sample', input: { action: 'sample', url: 'https://media.example.edu/courses/dsp/' }, output: { links: 14, untrusted_title: 'Signal processing, video lectures' }, state: 'ok', summary: '14 links on the page, 12 videos', ms: 950 },
        { kind: 'tool', id: itemId(), tool: 'zeus_probe', input: { url: 'https://media.example.edu/courses/dsp/video/lecture-01.mp4' }, output: { status: 206, size_bytes: 1288490188, resumable: true, requires_session: false }, state: 'ok', summary: '206, 1.20 GiB, resumable', ms: 310 },
        { kind: 'assistant', id: itemId(), text: 'The page links **12 lecture videos** (about 14 GiB in all). I checked the first one: it resumes and needs no login. I put them in a batch for you to review; nothing is downloading yet.' },
        {
          kind: 'card',
          id: itemId(),
          card: {
            type: 'batch',
            title: '12 lecture videos from media.example.edu',
            dest: 'D:\\Downloads\\Zeus\\Video\\Lectures',
            done: 'grabber',
            items: Array.from({ length: 12 }, (_, i) => ({ name: `lecture-${String(i + 1).padStart(2, '0')}-signal-processing.mp4`, size: Math.round((0.9 + ((i * 7) % 5) / 10) * 1024 ** 3), host: 'media.example.edu', url: `https://media.example.edu/courses/dsp/video/lecture-${String(i + 1).padStart(2, '0')}.mp4` })),
          },
        },
      ],
    },
  ]
}
